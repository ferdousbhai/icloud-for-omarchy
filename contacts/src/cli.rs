//! The command line: `icloud-contacts <command>` reads and changes iCloud
//! Contacts without GTK, through the same [`Service`] the app uses. Output
//! follows the shared product contract ([`icloud_session::output`]): text on
//! a terminal, the JSON envelope when piped,
//! `--json`/`--styled`/`--quiet`/`--ids-only`/`--count` to choose.
//!
//! Exit codes are the table every iCloud tool shares (docs/CLI.md,
//! `icloud_session::cli`): 0 ok; 1 error; 2 usage, an ambiguous name, or
//! `delete` without `--yes` when nobody can be asked; 3 not signed in, or
//! iCloud still refusing the session (`session_rejected`, HTTP 401/421);
//! 4 not found; 5 offline, `icloud-sessiond` unreachable, the keyring locked,
//! an unreadable answer or an iCloud server error (HTTP 5xx). The JSON `code`
//! tells them apart. A failure in a machine mode is
//! `{"ok":false,"error":{"code","message","hint"?}}` on stdout; in text mode
//! `error:`/`hint:` lines on stderr.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_NOT_FOUND, EXIT_UNAVAILABLE, EXIT_USAGE};
use icloud_session::output::{
    self, CliError, KEYRING_HINT, Mode, NETWORK_HINT, OutputFlags, Response, SERVICE_TROUBLE_HINT,
    SESSION_SERVICE_HINT, SIGN_IN_HINT,
};
use serde_json::{Value, json};

use crate::api::{self, SessionTransport};
use crate::model::{self, Change, Contact, Field, Multi};
use crate::service::Service;
use crate::store::{Cache, Store};
use crate::vcard;

const AFTER_HELP: &str = "\
CONTACT is a contact's ID, `me` (your own card), its exact name (ignoring
case), a unique part of it, an email address, or a phone number (matched by
its digits). An ambiguous CONTACT lists the matches; use the ID then.

PHONE, EMAIL and URL take an optional label first: work=+1 555 0100,
home=ann@example.com, WhatsApp=+62 812. Without one, a phone is mobile, an
email home and a URL homepage.

Reading commands sync first and fail with offline without a network;
--cached reads what the last sync left instead.

Output: human-readable text on a terminal, a JSON envelope
{\"ok\":true,\"data\",\"summary\",\"meta\"?} when piped. --json forces the
envelope, --styled text, --quiet prints only the data, --ids-only the
contact ids (list, show, add, edit, delete, export), --count the number of
results (contacts for list and export; changed contacts for sync). Flags
combine: ids-only > count > quiet > json > styled. A failure is
{\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or
error:/hint: lines on stderr in text mode.

Exit codes:
  0 ok
  1 error (cancelled, error, http 4xx)
  2 usage, ambiguous, or delete without --yes when nobody can be asked
  3 sign_in_required (icloud-session sign-in), session_rejected (iCloud
    keeps refusing the session although icloud-sessiond still holds it:
    sign in again)
  4 not_found
  5 offline, session_service (icloud-sessiond unreachable),
    keyring_unavailable, bad_response, http 5xx";

#[derive(Parser)]
#[command(
    name = "icloud-contacts",
    version,
    about = "iCloud Contacts from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Args {
    #[command(flatten)]
    out: OutputFlags,
    /// Keep the cache in DIR (default ~/.local/share/icloud-contacts).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Read the cache without syncing first.
    #[arg(long, global = true)]
    cached: bool,
    #[command(subcommand)]
    command: Command,
}

/// The fields `add` and `edit` set. An empty value removes the field.
#[derive(clap::Args)]
struct Fields {
    #[arg(long, value_name = "NAME")]
    first: Option<String>,
    #[arg(long, value_name = "NAME")]
    middle: Option<String>,
    #[arg(long, value_name = "NAME")]
    last: Option<String>,
    #[arg(long, value_name = "NAME")]
    nickname: Option<String>,
    #[arg(long, value_name = "NAME")]
    company: Option<String>,
    #[arg(long, value_name = "TEXT")]
    job_title: Option<String>,
    #[arg(long, value_name = "TEXT")]
    department: Option<String>,
    /// YYYY-MM-DD, or MM-DD without a year.
    #[arg(long, value_name = "DATE")]
    birthday: Option<String>,
    #[arg(long, value_name = "TEXT")]
    notes: Option<String>,
    /// Add a phone number (repeatable).
    #[arg(long = "phone", value_name = "PHONE")]
    phones: Vec<String>,
    /// Add an email address (repeatable).
    #[arg(long = "email", value_name = "EMAIL")]
    emails: Vec<String>,
    /// Add a URL (repeatable).
    #[arg(long = "url", value_name = "URL")]
    urls: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Every contact, or those matching QUERY, sorted by last name.
    ///
    /// QUERY matches a name, nickname, company or email, or (digits only)
    /// a phone number.
    ///
    /// JSON: [contact], each {id, name, first, middle, last, nickname,
    /// company, job_title, department, is_company, me, phones: [{label,
    /// value}], emails, urls, addresses: [{label, street, city, state,
    /// postal_code, country, country_code}], birthday, notes, has_photo}
    #[command(after_help = AFTER_HELP)]
    List {
        #[arg(value_name = "QUERY")]
        query: Option<String>,
    },
    /// One contact, with everything it holds.
    ///
    /// JSON: the contact, as in `list`.
    #[command(after_help = AFTER_HELP)]
    Show {
        #[arg(value_name = "CONTACT")]
        contact: String,
    },
    /// Add a contact.
    ///
    /// It needs a name, a company, a phone or an email.
    ///
    /// JSON: {action: "add", contact}
    #[command(after_help = AFTER_HELP)]
    Add {
        #[command(flatten)]
        fields: Fields,
        /// Show it as a company (by --company, not a person's name).
        #[arg(long)]
        is_company: bool,
    },
    /// Change a contact's fields, add phones, emails and URLs, or remove them.
    ///
    /// What it does not name stays as it is, including what this tool does
    /// not show (photo, social profiles, related names). An empty value
    /// (--nickname "") removes a field.
    ///
    /// JSON: {action: "edit", contact}
    #[command(after_help = AFTER_HELP)]
    Edit {
        #[arg(value_name = "CONTACT")]
        contact: String,
        #[command(flatten)]
        fields: Fields,
        /// Remove a phone number, matched by its digits (repeatable).
        #[arg(long = "remove-phone", value_name = "PHONE")]
        remove_phones: Vec<String>,
        /// Remove an email address (repeatable).
        #[arg(long = "remove-email", value_name = "EMAIL")]
        remove_emails: Vec<String>,
        /// Remove a URL (repeatable).
        #[arg(long = "remove-url", value_name = "URL")]
        remove_urls: Vec<String>,
    },
    /// Delete a contact, on every device.
    ///
    /// iCloud has no Recently Deleted for contacts: only icloud.com's Data
    /// Recovery (Restore Contacts, a whole earlier version of the address
    /// book) brings one back. Asks in text mode on a terminal (stdin and
    /// stdout); anywhere else --yes is required (a usage error, exit 2,
    /// otherwise).
    ///
    /// JSON: {action: "delete", contact: {id, name}}
    #[command(after_help = AFTER_HELP)]
    Delete {
        #[arg(value_name = "CONTACT")]
        contact: String,
        /// Do not ask; required when nobody can be asked (not a terminal, or a machine output mode).
        #[arg(long, short)]
        yes: bool,
    },
    /// Write contacts as vCard 3.0: every one, or those named.
    ///
    /// To --output FILE, or to stdout: the vCards themselves in text mode
    /// (--styled when piped), inside the JSON otherwise. Photos are not
    /// included.
    ///
    /// JSON: {action: "export", count, file?, vcard?} (vcard when there is no --output)
    #[command(after_help = AFTER_HELP)]
    Export {
        #[arg(value_name = "CONTACT")]
        contacts: Vec<String>,
        #[arg(long, short, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Fetch every contact from iCloud.
    ///
    /// JSON: {contacts, changed}
    #[command(after_help = AFTER_HELP)]
    Sync,
}

/// A failure: the contract's error object and exit code.
type Failure = CliError;

type Outcome = Result<Response, Failure>;

fn usage(msg: impl Into<String>) -> Failure {
    CliError::new("usage", EXIT_USAGE, msg)
}

fn other(msg: impl Into<String>) -> Failure {
    CliError::new("error", EXIT_ERROR, msg)
}

/// The shared sign-in hint, plus the app's own button.
fn sign_in_hint() -> String {
    format!("{SIGN_IN_HINT}, or Sign In in the app")
}

/// A Contacts failure as the contract's code, exit code, message and hint,
/// worded as `icloud_session::output` words it in every iCloud tool.
impl From<api::Error> for Failure {
    fn from(e: api::Error) -> Self {
        use api::Error as A;
        let msg = e.to_string();
        match e {
            A::SignInRequired => output::sign_in_required().hint(sign_in_hint()),
            A::Http { status, .. } => {
                let e = output::http_failure(status, msg);
                if e.hint.as_deref() == Some(SIGN_IN_HINT) {
                    e.hint(sign_in_hint())
                } else {
                    e
                }
            }
            A::Offline(_) => CliError::new("offline", EXIT_UNAVAILABLE, msg).hint(NETWORK_HINT),
            A::Service(_) => CliError::new("session_service", EXIT_UNAVAILABLE, msg).hint(SESSION_SERVICE_HINT),
            A::KeyringUnavailable(_) => CliError::new("keyring_unavailable", EXIT_UNAVAILABLE, msg).hint(KEYRING_HINT),
            A::BadResponse(_) => CliError::new("bad_response", EXIT_UNAVAILABLE, msg).hint(SERVICE_TROUBLE_HINT),
            A::Refused { .. } | A::Other(_) => other(msg),
        }
    }
}

/// "1 contact", "N contacts".
fn contacts(n: usize) -> String {
    match n {
        1 => "1 contact".into(),
        n => format!("{n} contacts"),
    }
}

/// Runs the command line on the process arguments and returns the exit code.
pub fn run() -> u8 {
    let args = match output::parse::<Args>() {
        Ok(args) => args,
        Err(code) => return code,
    };
    let mode = args.out.mode();
    let transport = SessionTransport::default();
    let result = (|| {
        let dir = args
            .data_dir
            .or_else(Store::default_dir)
            .ok_or_else(|| other("no data directory (no $XDG_DATA_HOME or $HOME)").hint("pass --data-dir DIR"))?;
        let cli = Cli {
            mode,
            cached: args.cached,
            svc: Service::new(&transport, Store::new(dir)),
        };
        cli.dispatch(args.command)
    })();
    match result {
        Ok(resp) => resp.print(mode),
        Err(e) => e.report(mode),
    }
}

struct Cli<'a> {
    mode: Mode,
    cached: bool,
    svc: Service<'a>,
}

impl Cli<'_> {
    fn dispatch(&self, command: Command) -> Outcome {
        match command {
            Command::List { query } => self.list(query.as_deref()),
            Command::Show { contact } => self.show(&contact),
            Command::Add { fields, is_company } => self.add(fields, is_company),
            Command::Edit {
                contact,
                fields,
                remove_phones,
                remove_emails,
                remove_urls,
            } => {
                let mut changes = changes(fields)?;
                for (m, values) in [
                    (Multi::Phone, remove_phones),
                    (Multi::Email, remove_emails),
                    (Multi::Url, remove_urls),
                ] {
                    changes.extend(values.into_iter().map(|v| Change::Remove(m, v)));
                }
                self.edit(&contact, changes)
            }
            Command::Delete { contact, yes } => self.delete(&contact, yes),
            Command::Export { contacts, output } => self.export(&contacts, output),
            Command::Sync => self.sync(),
        }
    }

    /// The cache after a sync, or as the last sync left it with `--cached`.
    fn fresh(&self) -> Result<Cache, Failure> {
        Ok(if self.cached {
            self.svc.cache()?
        } else {
            self.svc.sync()?.1
        })
    }

    fn list(&self, query: Option<&str>) -> Outcome {
        let cache = self.fresh()?;
        let rows: Vec<&Contact> = cache
            .contacts
            .iter()
            .filter(|c| query.is_none_or(|q| c.matches(q)))
            .collect();
        let data: Vec<Value> = rows.iter().map(|c| contact_json(c, cache.is_me(c))).collect();
        let ids: Vec<String> = rows.iter().map(|c| c.id().to_owned()).collect();
        let human = if rows.is_empty() {
            "No contacts.".to_string()
        } else {
            rows.iter().map(|c| line(c)).collect::<String>()
        };
        Ok(Response::new(data, contacts(rows.len()))
            .human(human.trim_end())
            .ids(ids))
    }

    fn show(&self, query: &str) -> Outcome {
        let cache = self.fresh()?;
        let c = resolve(&cache, query)?;
        let me = cache.is_me(c);
        Ok(Response::new(contact_json(c, me), c.display_name())
            .human(detail(c, me).trim_end())
            .ids(vec![c.id().to_owned()]))
    }

    fn add(&self, fields: Fields, is_company: bool) -> Outcome {
        let mut changes = changes(fields)?;
        // Empty values mean "remove", which a new card has nothing to.
        changes.retain(|c| !matches!(c, Change::Set(_, v) if v.is_empty()));
        if !model::names_someone(&changes) {
            return Err(usage(
                "add needs a name (--first, --last, --company, ...), a --phone or an --email",
            ));
        }
        if is_company && !changes.iter().any(|c| matches!(c, Change::Set(Field::Company, _))) {
            return Err(usage("--is-company needs --company"));
        }
        let saved = self.svc.add(&changes, is_company)?;
        let c = saved.value;
        // A new card is never the "me" card.
        Ok(done(
            "add",
            &c,
            false,
            &format!("Added {}", c.display_name()),
            saved.warning,
        ))
    }

    fn edit(&self, query: &str, changes: Vec<Change>) -> Outcome {
        if changes.is_empty() {
            return Err(usage(
                "edit needs a field to set (--last, --phone, --remove-email, ...)",
            ));
        }
        let cache = self.fresh()?;
        let c = resolve(&cache, query)?.clone();
        let saved = self.svc.update(&c, &changes)?;
        let after = saved.value;
        Ok(done(
            "edit",
            &after,
            cache.is_me(&c),
            &format!("Changed {}", after.display_name()),
            saved.warning,
        ))
    }

    fn delete(&self, query: &str, yes: bool) -> Outcome {
        let cache = self.fresh()?;
        let c = resolve(&cache, query)?.clone();
        let name = c.display_name();
        confirm(
            self.mode,
            "delete",
            yes,
            &format!("Delete {name} from iCloud, on every device?"),
        )?;
        let saved = self.svc.delete(&c)?;
        let mut data = json!({ "action": "delete", "contact": { "id": c.id(), "name": name } });
        warn(&mut data, saved.warning);
        Ok(Response::new(data, format!("Deleted {name}")).ids(vec![c.id().to_owned()]))
    }

    fn export(&self, queries: &[String], output: Option<PathBuf>) -> Outcome {
        let cache = self.fresh()?;
        let chosen: Vec<&Contact> = if queries.is_empty() {
            cache.contacts.iter().collect()
        } else {
            queries.iter().map(|q| resolve(&cache, q)).collect::<Result<_, _>>()?
        };
        let text: String = chosen.iter().map(|c| vcard::card(c)).collect();
        let count = chosen.len();
        let ids: Vec<String> = chosen.iter().map(|c| c.id().to_owned()).collect();
        let resp = match output {
            Some(path) => {
                std::fs::write(&path, &text).map_err(|e| other(format!("cannot write {}: {e}", path.display())))?;
                Response::new(
                    json!({ "action": "export", "count": count, "file": path }),
                    format!("Wrote {} to {}", contacts(count), path.display()),
                )
            }
            // Text mode into a pipe or a file (`--styled`): the vCards as
            // they are, CRLF line ends and all.
            None if self.mode == Mode::Human && !io::stdout().is_terminal() => {
                io::stdout()
                    .write_all(text.as_bytes())
                    .map_err(|e| other(e.to_string()))?;
                return Ok(Response::silent());
            }
            None => {
                let human = text.replace('\r', "");
                Response::new(
                    json!({ "action": "export", "count": count, "vcard": text }),
                    format!("Exported {}", contacts(count)),
                )
                .human(human.trim_end())
            }
        };
        Ok(resp.ids(ids))
    }

    fn sync(&self) -> Outcome {
        let (report, _) = self.svc.sync()?;
        let data = json!({ "contacts": report.contacts, "changed": report.changed });
        let text = format!("Synced: {} ({} changed)", contacts(report.contacts), report.changed);
        Ok(Response::new(data, text).count(report.changed))
    }
}

/// What `add` and `edit` answer: the contact as saved (`me`: it is the "me"
/// card).
fn done(action: &str, c: &Contact, me: bool, text: &str, warning: Option<String>) -> Response {
    let mut data = json!({ "action": action, "contact": contact_json(c, me) });
    warn(&mut data, warning);
    Response::new(data, text.to_string()).ids(vec![c.id().to_owned()])
}

/// The changes `fields` asks for, checked.
fn changes(f: Fields) -> Result<Vec<Change>, Failure> {
    let mut out = Vec::new();
    let birthday = f
        .birthday
        .map(|b| {
            if b.trim().is_empty() {
                Ok(String::new())
            } else {
                model::parse_birthday(&b)
            }
        })
        .transpose()
        .map_err(usage)?;
    for (field, v) in [
        (Field::First, f.first),
        (Field::Middle, f.middle),
        (Field::Last, f.last),
        (Field::Nickname, f.nickname),
        (Field::Company, f.company),
        (Field::JobTitle, f.job_title),
        (Field::Department, f.department),
        (Field::Birthday, birthday),
    ] {
        if let Some(v) = v {
            out.push(Change::Set(field, v.trim().to_owned()));
        }
    }
    if let Some(n) = f.notes {
        out.push(Change::Set(Field::Notes, n));
    }
    for (m, values) in [(Multi::Phone, f.phones), (Multi::Email, f.emails), (Multi::Url, f.urls)] {
        for v in values {
            let l = model::parse_labeled(&v, m.default_label());
            m.check(&l.value).map_err(usage)?;
            out.push(Change::Add(m, l));
        }
    }
    Ok(out)
}

/// Asks unless `yes`, and only when someone can answer
/// ([`output::interactive`]: text mode, stdin and stdout a terminal);
/// otherwise refuses (exit 2) without `--yes`.
fn confirm(mode: Mode, cmd: &str, yes: bool, question: &str) -> Result<(), Failure> {
    if yes {
        return Ok(());
    }
    if !output::interactive(mode) {
        return Err(
            usage(format!("{cmd} asks before it acts, and nobody can be asked here"))
                .hint(format!("pass --yes to {cmd} without asking")),
        );
    }
    if cli::confirm(question) {
        Ok(())
    } else {
        Err(CliError::new("cancelled", EXIT_ERROR, "cancelled"))
    }
}

/// A write that happened but left a problem behind: on stderr, and as
/// `warning` in the data. The command still succeeds.
fn warn(data: &mut Value, warning: Option<String>) {
    if let Some(w) = warning {
        eprintln!("{}", output::terminal_safe(&format!("warning: {w}")));
        data["warning"] = Value::String(w);
    }
}

/// Finds a contact by ID or `me`, else by name (ignoring case), email or
/// phone number, else by a unique part of its name.
fn resolve<'c>(cache: &'c Cache, query: &str) -> Result<&'c Contact, Failure> {
    let q = query.trim();
    if let Some(c) = cache.get(q) {
        return Ok(c);
    }
    if q.eq_ignore_ascii_case("me") {
        return cache.me.as_deref().and_then(|id| cache.get(id)).ok_or_else(|| {
            CliError::new(
                "not_found",
                EXIT_NOT_FOUND,
                "iCloud has no \"me\" card for this account",
            )
            .hint("set My Card on an Apple device")
        });
    }
    let f = model::fold(q);
    let all = &cache.contacts;
    let pick = |keep: &dyn Fn(&Contact) -> bool| all.iter().filter(|c| keep(c)).collect::<Vec<_>>();
    let mut matches = pick(&|c| model::fold(&c.display_name()) == f || model::fold(&c.person_name()) == f);
    if matches.is_empty() && q.contains('@') {
        matches = pick(&|c| c.values(Multi::Email).iter().any(|e| Multi::Email.matches(&e.value, q)));
    }
    let d = model::digits(q);
    if matches.is_empty() && d.len() >= 3 && !q.chars().any(char::is_alphabetic) {
        // By the number's tail, so +1 555 0100 finds 555-0100 and the reverse.
        matches = pick(&|c| {
            c.values(Multi::Phone).iter().any(|p| {
                let pd = model::digits(&p.value);
                pd.ends_with(&d) || model::same_number(&p.value, q)
            })
        });
    }
    if matches.is_empty() && !f.is_empty() {
        matches = pick(&|c| model::fold(&c.display_name()).contains(&f));
    }
    match matches.as_slice() {
        [c] => Ok(c),
        [] => Err(
            CliError::new("not_found", EXIT_NOT_FOUND, format!("no contact matches \"{query}\""))
                .hint("`icloud-contacts list QUERY` searches names, companies, emails and phone numbers"),
        ),
        many => Err(CliError::new(
            "ambiguous",
            EXIT_USAGE,
            format!("\"{query}\" matches {} contacts", many.len()),
        )
        .hint(format!(
            "name one exactly or use its ID:{}",
            many.iter()
                .take(20)
                .map(|c| format!("\n  {}  ({})", c.display_name(), c.id()))
                .collect::<String>()
        ))),
    }
}

fn labeled_json(ls: Vec<model::Labeled>) -> Value {
    json!(
        ls.into_iter()
            .map(|l| json!({ "label": model::label_display(&l.label), "value": l.value }))
            .collect::<Vec<_>>()
    )
}

fn opt(s: &str) -> Value {
    if s.is_empty() { Value::Null } else { json!(s) }
}

/// `me`: whether `c` is the account's "me" card.
pub fn contact_json(c: &Contact, me: bool) -> Value {
    let addresses: Vec<Value> = c
        .addresses()
        .into_iter()
        .map(|a| {
            json!({
                "label": model::label_display(&a.label),
                "street": opt(&a.street),
                "city": opt(&a.city),
                "state": opt(&a.state),
                "postal_code": opt(&a.postal_code),
                "country": opt(&a.country),
                "country_code": opt(&a.country_code),
            })
        })
        .collect();
    let birthday = c.field(Field::Birthday);
    json!({
        "id": c.id(),
        "name": c.display_name(),
        "first": opt(c.field(Field::First)),
        "middle": opt(c.field(Field::Middle)),
        "last": opt(c.field(Field::Last)),
        "nickname": opt(c.field(Field::Nickname)),
        "company": opt(c.field(Field::Company)),
        "job_title": opt(c.field(Field::JobTitle)),
        "department": opt(c.field(Field::Department)),
        "is_company": c.is_company(),
        "me": me,
        "phones": labeled_json(c.values(Multi::Phone)),
        "emails": labeled_json(c.values(Multi::Email)),
        "urls": labeled_json(c.values(Multi::Url)),
        "addresses": addresses,
        "birthday": opt(&model::birthday_display(birthday)),
        "notes": opt(c.field(Field::Notes)),
        "has_photo": c.has_photo(),
    })
}

fn line(c: &Contact) -> String {
    let mut extra = Vec::new();
    let company = c.field(Field::Company);
    if !company.is_empty() && !c.is_company() {
        extra.push(company.to_owned());
    }
    if let Some(p) = c.values(Multi::Phone).into_iter().next() {
        extra.push(p.value);
    }
    if let Some(e) = c.values(Multi::Email).into_iter().next() {
        extra.push(e.value);
    }
    if extra.is_empty() {
        format!("{}\n", c.display_name())
    } else {
        format!("{}  ({})\n", c.display_name(), extra.join(", "))
    }
}

fn detail(c: &Contact, me: bool) -> String {
    let mut out = format!("{}{}\n", c.display_name(), if me { "  (me)" } else { "" });
    let row = |out: &mut String, label: &str, value: &str| {
        if !value.is_empty() {
            *out += &format!("  {:<11}{value}\n", format!("{label}:"));
        }
    };
    let job = c.work(", ");
    row(&mut out, "nickname", c.field(Field::Nickname));
    row(&mut out, "work", &job);
    for (m, noun) in [(Multi::Phone, "phone"), (Multi::Email, "email"), (Multi::Url, "url")] {
        for l in c.values(m) {
            row(
                &mut out,
                noun,
                &format!("{}  ({})", l.value, model::label_display(&l.label)),
            );
        }
    }
    for a in c.addresses() {
        let place = [a.street.replace('\n', ", "), a.city, a.state, a.postal_code, a.country]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        row(
            &mut out,
            "address",
            &format!("{place}  ({})", model::label_display(&a.label)),
        );
    }
    row(&mut out, "birthday", &model::birthday_display(c.field(Field::Birthday)));
    row(&mut out, "id", c.id());
    let notes = c.field(Field::Notes);
    if !notes.is_empty() {
        out += "\n";
        for l in notes.lines() {
            out += &format!("  {l}\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

    #[test]
    fn failures_use_the_shared_codes_exits_and_hints() {
        use api::Error as A;
        let http = |status| A::Http {
            status,
            body: String::new(),
        };
        let cases = [
            (http(421), "session_rejected", 3),
            (http(401), "session_rejected", 3),
            (http(503), "http", 5),
            (http(404), "http", 1),
            (A::Offline("x".into()), "offline", 5),
            (A::Service("x".into()), "session_service", 5),
            (A::KeyringUnavailable("x".into()), "keyring_unavailable", 5),
            (A::BadResponse("x".into()), "bad_response", 5),
            (
                A::Refused {
                    code: 412,
                    message: "x".into(),
                },
                "error",
                1,
            ),
            (A::Other("x".into()), "error", 1),
        ];
        for (e, code, exit) in cases {
            let what = format!("{e:?}");
            let f = Failure::from(e);
            assert_eq!((f.code.as_str(), f.exit), (code, exit), "{what}");
            assert!(exit != 5 || f.hint.is_some(), "{what}");
        }
        let f = Failure::from(http(421));
        assert_eq!(f.hint.as_deref(), Some(sign_in_hint().as_str()));
        let f = Failure::from(A::Offline("x".into()));
        assert_eq!(f.hint.as_deref(), Some(NETWORK_HINT));
    }
}
