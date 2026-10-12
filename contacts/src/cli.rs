//! The command line: `icloud-contacts <command>` reads and changes iCloud
//! Contacts without GTK, through the same [`Service`] the app uses.
//! Human-readable output by default, `--json` for machines.
//!
//! Exit codes are the table every iCloud tool shares (docs/CLI.md,
//! `icloud_session::cli`): 0 ok, 1 error, 2 sign-in required, 64 usage.
//! With `--json` an error is one JSON line on stderr,
//! `{"error":{"code","message","exit_code"}}`.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_OK, EXIT_SIGN_IN, EXIT_USAGE};
use serde_json::{Value, json};

use crate::api::{self, SessionTransport};
use crate::model::{self, Change, Contact, Field, Multi};
use crate::service::Service;
use crate::store::{Cache, Store};
use crate::vcard;

const TOOL: &str = "icloud-contacts";

const AFTER_HELP: &str = "\
CONTACT is a contact's ID, `me` (your own card), its exact name (ignoring
case), a unique part of it, an email address, or a phone number (matched by
its digits).

PHONE, EMAIL and URL take an optional label first: work=+1 555 0100,
home=ann@example.com, WhatsApp=+62 812. Without one, a phone is mobile, an
email home and a URL homepage.

Reading commands sync first and fail with offline without a network;
--cached reads what the last sync left instead.

With --json, stdout is only the JSON result and an error is one JSON line
on stderr: {\"error\":{\"code\",\"message\",\"exit_code\"}}; codes: usage,
sign_in_required, offline, keyring_unavailable (the keyring holding the
session is locked: unlock it), bad_response (iCloud answered with something
unreadable), not_found, ambiguous, cancelled, error.

Exit codes: 0 ok, 1 error, 2 sign-in required (icloud-session sign-in),
64 usage.";

#[derive(Parser)]
#[command(
    name = "icloud-contacts",
    version,
    about = "iCloud Contacts from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Args {
    /// Machine-readable output on stdout (errors as JSON on stderr).
    #[arg(long, global = true)]
    json: bool,
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
    /// book) brings one back. Asks on a terminal; without one, --yes is
    /// required.
    ///
    /// JSON: {action: "delete", contact: {id, name}}
    #[command(after_help = AFTER_HELP)]
    Delete {
        #[arg(value_name = "CONTACT")]
        contact: String,
        /// Do not ask; required when stdin is not a terminal.
        #[arg(long, short)]
        yes: bool,
    },
    /// Write contacts as vCard 3.0: every one, or those named.
    ///
    /// To stdout, or to --output FILE. Photos are not included.
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

enum Failure {
    Usage(String),
    Cloud(api::Error),
    /// A machine-readable code (`not_found`, `ambiguous`, `cancelled`) and
    /// its message.
    Coded(&'static str, String),
    Other(String),
}

impl From<api::Error> for Failure {
    fn from(e: api::Error) -> Self {
        Failure::Cloud(e)
    }
}

impl Failure {
    fn code(&self) -> u8 {
        match self {
            Failure::Usage(_) => EXIT_USAGE,
            Failure::Cloud(api::Error::SignInRequired) => EXIT_SIGN_IN,
            _ => EXIT_ERROR,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Failure::Usage(_) => "usage",
            Failure::Cloud(api::Error::SignInRequired) => "sign_in_required",
            Failure::Cloud(api::Error::Offline(_)) => "offline",
            Failure::Cloud(api::Error::KeyringUnavailable(_)) => "keyring_unavailable",
            Failure::Cloud(api::Error::BadResponse(_)) => "bad_response",
            Failure::Coded(code, _) => code,
            _ => "error",
        }
    }

    fn message(&self) -> String {
        match self {
            Failure::Usage(m) | Failure::Other(m) | Failure::Coded(_, m) => m.clone(),
            Failure::Cloud(api::Error::SignInRequired) => format!(
                "{}; run `icloud-session sign-in` (or Sign In in the app)",
                icloud_session::sign_in_required_message()
            ),
            Failure::Cloud(e) => e.to_string(),
        }
    }
}

type Outcome = Result<(), Failure>;

fn usage(msg: impl Into<String>) -> Failure {
    Failure::Usage(msg.into())
}

/// Runs the command line on the process arguments and returns the exit code.
pub fn run() -> u8 {
    let args = match cli::parse::<Args>(TOOL) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let json = args.json;
    let transport = SessionTransport::default();
    let result = (|| {
        let dir = args
            .data_dir
            .or_else(Store::default_dir)
            .ok_or_else(|| Failure::Other("no data directory: pass --data-dir".into()))?;
        let cli = Cli {
            json,
            cached: args.cached,
            svc: Service::new(&transport, Store::new(dir)),
        };
        cli.dispatch(args.command)
    })();
    match result {
        Ok(()) => EXIT_OK,
        Err(f) => cli::report(TOOL, json, f.kind(), f.code(), &f.message(), None),
    }
}

struct Cli<'a> {
    json: bool,
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

    fn print(&self, v: Value, text: impl FnOnce() -> String) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(&v).expect("JSON values serialize"));
        } else {
            print!("{}", text());
        }
    }

    fn list(&self, query: Option<&str>) -> Outcome {
        let cache = self.fresh()?;
        let rows: Vec<&Contact> = cache
            .contacts
            .iter()
            .filter(|c| query.is_none_or(|q| c.matches(q)))
            .collect();
        if self.json {
            let json_rows: Vec<Value> = rows.iter().map(|c| contact_json(c, &cache)).collect();
            self.print(json!(json_rows), String::new);
        } else if rows.is_empty() {
            println!("No contacts.");
        } else {
            print!("{}", rows.iter().map(|c| line(c)).collect::<String>());
        }
        Ok(())
    }

    fn show(&self, query: &str) -> Outcome {
        let cache = self.fresh()?;
        let c = resolve(&cache, query)?;
        self.print(contact_json(c, &cache), || detail(c, &cache));
        Ok(())
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
        self.done("add", &c, &format!("Added {}", c.display_name()), saved.warning)
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
        self.done(
            "edit",
            &after,
            &format!("Changed {}", after.display_name()),
            saved.warning,
        )
    }

    fn delete(&self, query: &str, yes: bool) -> Outcome {
        let cache = self.fresh()?;
        let c = resolve(&cache, query)?.clone();
        let name = c.display_name();
        confirm(yes, &format!("Delete {name} from iCloud, on every device?"))?;
        let saved = self.svc.delete(&c)?;
        let mut out = json!({ "action": "delete", "contact": { "id": c.id(), "name": name } });
        warn(&mut out, saved.warning);
        self.print(out, || format!("Deleted {name}\n"));
        Ok(())
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
        match output {
            Some(path) => {
                std::fs::write(&path, &text)
                    .map_err(|e| Failure::Other(format!("cannot write {}: {e}", path.display())))?;
                let out = json!({ "action": "export", "count": count, "file": path });
                self.print(out, || format!("Wrote {count} contact(s) to {}\n", path.display()));
            }
            None if self.json => self.print(
                json!({ "action": "export", "count": count, "vcard": text }),
                String::new,
            ),
            None => {
                io::stdout()
                    .write_all(text.as_bytes())
                    .map_err(|e| Failure::Other(e.to_string()))?;
            }
        }
        Ok(())
    }

    fn done(&self, action: &str, c: &Contact, text: &str, warning: Option<String>) -> Outcome {
        let cache = self.svc.cache()?;
        let mut out = json!({ "action": action, "contact": contact_json(c, &cache) });
        warn(&mut out, warning);
        self.print(out, || format!("{text}\n"));
        Ok(())
    }

    fn sync(&self) -> Outcome {
        let (report, _) = self.svc.sync()?;
        let out = json!({ "contacts": report.contacts, "changed": report.changed });
        self.print(out, || {
            format!("Synced: {} contacts ({} changed)\n", report.contacts, report.changed)
        });
        Ok(())
    }
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
        .map_err(Failure::Usage)?;
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
            m.check(&l.value).map_err(Failure::Usage)?;
            out.push(Change::Add(m, l));
        }
    }
    Ok(out)
}

/// Asks on a terminal unless `yes`; refuses without a terminal.
fn confirm(yes: bool, question: &str) -> Outcome {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(usage(
            "delete asks before it acts: pass --yes when stdin is not a terminal",
        ));
    }
    if cli::confirm(question) {
        Ok(())
    } else {
        Err(Failure::Coded("cancelled", "cancelled".into()))
    }
}

/// A write that happened but left a problem behind: on stderr, and as
/// `warning` in the JSON. The command still succeeds.
fn warn(out: &mut Value, warning: Option<String>) {
    if let Some(w) = warning {
        eprintln!("{TOOL}: {w}");
        out["warning"] = Value::String(w);
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
            Failure::Coded(
                "not_found",
                "iCloud has no \"me\" card for this account (set My Card on an Apple device)".into(),
            )
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
        [] => Err(Failure::Coded("not_found", format!("no contact matches \"{query}\""))),
        many => Err(Failure::Coded(
            "ambiguous",
            format!(
                "\"{query}\" matches {} contacts; name one exactly or use its ID:{}",
                many.len(),
                many.iter()
                    .take(20)
                    .map(|c| format!("\n  {}  ({})", c.display_name(), c.id()))
                    .collect::<String>()
            ),
        )),
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

pub fn contact_json(c: &Contact, cache: &Cache) -> Value {
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
        "me": cache.is_me(c),
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

fn detail(c: &Contact, cache: &Cache) -> String {
    let mut out = format!("{}{}\n", c.display_name(), if cache.is_me(c) { "  (me)" } else { "" });
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

    fn cache() -> Cache {
        let card = |v: Value| Contact(v.as_object().unwrap().clone());
        let mut c = Cache {
            me: Some("ME".into()),
            contacts: vec![
                card(json!({"contactId": "ME", "firstName": "Ferdous"})),
                card(
                    json!({"contactId": "A", "firstName": "Ann", "lastName": "Lee", "emailAddresses": [{"label": "HOME", "field": "ann@x.org"}]}),
                ),
                card(
                    json!({"contactId": "B", "firstName": "Anna", "lastName": "Berg", "phones": [{"label": "MOBILE", "field": "+47 555-0100"}]}),
                ),
                card(json!({"contactId": "C", "firstName": "Bob"})),
            ],
            ..Cache::default()
        };
        c.sort();
        c
    }

    #[test]
    fn resolves_ids_me_names_emails_phones_and_parts() {
        let c = cache();
        let id = |q: &str| resolve(&c, q).ok().map(|c| c.id().to_owned());
        assert_eq!(id("B").as_deref(), Some("B"));
        assert_eq!(id("me").as_deref(), Some("ME"));
        assert_eq!(id("ann lee").as_deref(), Some("A"));
        assert_eq!(id("ANN@x.org").as_deref(), Some("A"));
        assert_eq!(id("5550100").as_deref(), Some("B"));
        assert_eq!(id("bo").as_deref(), Some("C"));
        assert!(matches!(resolve(&c, "ann"), Err(Failure::Coded("ambiguous", _))));
        assert!(matches!(resolve(&c, "zed"), Err(Failure::Coded("not_found", _))));
    }
}
