//! `icloud-session status | sign-in | authorize-find-my | set-password |
//! forget-password | sign-out | validate`
//!
//! Talks to icloud-sessiond over D-Bus. JSON on stdout; errors on stderr
//! with a non-zero exit, in the table every iCloud tool shares: 1 error,
//! 2 sign-in required, 4 Find My not authorized, 64 usage. With `--json`
//! an error is one JSON line on stderr:
//! `{"error":{"code","message","exit_code","hint"?}}`.

use std::io::{IsTerminal, Read};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use icloud_session::{BUS_NAME, Error, INTERFACE, OBJECT_PATH, Session, Status};
use serde_json::json;
use zeroize::Zeroizing;

const EXIT_ERROR: u8 = 1;
const EXIT_SIGN_IN: u8 = 2;
const EXIT_FIND_MY_AUTH: u8 = 4;
const EXIT_USAGE: u8 = 64;

/// `--json` anywhere on the command line: errors as JSON (the output
/// already is).
static JSON: AtomicBool = AtomicBool::new(false);

/// Each command's name, its synopsis and what it does (`<command> --help`).
const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "status",
        "status",
        "The daemon's properties: signed_in, apple_id, dsid, expires_at (Unix seconds; null for a\n\
         sign-in without \"Keep me signed in\"), signing_in, find_my_authorized, find_my_password_stored.",
    ),
    (
        "sign-in",
        "sign-in [--no-wait]",
        "Open the sign-in window (Apple's page: password and 2FA, typed by a person) and wait until it\n\
         closes, then print status. Exits 2 when the window closed without a sign-in. --no-wait prints\n\
         status at once instead; poll `status` until signing_in is false.",
    ),
    (
        "authorize-find-my",
        "authorize-find-my [--no-wait]",
        "Open the sign-in window on Find My, where Apple asks for the password again, and wait until it\n\
         closes, then print status. Exits 4 when Find My is still not authorized. --no-wait as for sign-in.",
    ),
    (
        "set-password",
        "set-password [--from-bitwarden [ITEM] | --from-1password [ITEM]]",
        "Store the Apple ID password in the keyring (GNOME Keyring), so the daemon re-authorizes Find My\n\
         by itself when it asks for the password again. The password is checked with a Find My sign-in\n\
         first and stored only if Apple accepts it. It is read from the terminal without echo, from stdin\n\
         when that is not a terminal, or once from Bitwarden (`bw get password`) or 1Password (`op`).\n\
         Without ITEM: the login saved for apple.com or icloud.com; with 1Password, ITEM may be an op://\n\
         reference. A locked Bitwarden is unlocked on the terminal (export BW_SESSION to skip that).",
    ),
    (
        "forget-password",
        "forget-password",
        "Remove the stored password from the keyring, then print status.",
    ),
    (
        "sign-out",
        "sign-out",
        "Forget the account and the sign-in window's profile, then print status (a stored password stays\n\
         until forget-password). Every iCloud app is signed out.",
    ),
    (
        "validate",
        "validate",
        "The session's webservices: {dsid, apple_id, webservices} (the daemon revalidates when older than\n\
         10 minutes). Exits 2 when Apple no longer accepts the sign-in.",
    ),
];

const FOOTER: &str = "--json: errors as one JSON line on stderr, {\"error\":{\"code\",\"message\",\"exit_code\"}} (output is JSON anyway).
Exit codes: 0 ok, 1 error, 2 sign-in required (or sign-in not completed), 4 Find My not authorized
(or its authorization not completed), 64 usage.";

fn usage_text() -> String {
    let mut out = String::from("usage: icloud-session <command> [--json]\n\ncommands:\n");
    for (_, synopsis, help) in COMMANDS {
        out.push_str(&format!("  {synopsis}\n"));
        for line in help.lines() {
            out.push_str(&format!("      {}\n", line.trim_start()));
        }
    }
    out.push_str("\n`icloud-session <command> --help` shows one command.\n");
    out.push_str(FOOTER);
    out
}

fn command_help(name: &str) -> Option<String> {
    let (_, synopsis, help) = COMMANDS.iter().find(|(n, ..)| *n == name)?;
    let body: Vec<&str> = help.lines().map(str::trim_start).collect();
    Some(format!(
        "usage: icloud-session {synopsis} [--json]\n\n{}\n\n{FOOTER}",
        body.join("\n")
    ))
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    JSON.store(raw.iter().any(|a| a == "--json"), Ordering::Relaxed);
    let args: Vec<&str> = raw.iter().map(String::as_str).filter(|a| *a != "--json").collect();
    let wants_help = args.iter().any(|a| matches!(*a, "-h" | "--help"));
    match args.as_slice() {
        ["-h" | "--help" | "help"] => {
            println!("{}", usage_text());
            return ExitCode::SUCCESS;
        }
        ["help", name] | [name, ..] if wants_help || args.first() == Some(&"help") => {
            return match command_help(name) {
                Some(text) => {
                    println!("{text}");
                    ExitCode::SUCCESS
                }
                None => usage(&format!("unknown command \"{name}\"")),
            };
        }
        _ => {}
    }
    match args.as_slice() {
        ["status"] => run(icloud_session::status().map(|s| status_json(&s))),
        ["sign-in", rest @ ..] => match no_wait(rest) {
            Some(true) => run(open_now(icloud_session::sign_in)),
            Some(false) => match open_window(icloud_session::sign_in) {
                Ok(s) if s.signed_in => print(&status_json(&s)),
                Ok(s) => {
                    println!("{}", status_json(&s));
                    fail_with("sign_in_not_completed", EXIT_SIGN_IN, "sign-in did not complete", None)
                }
                Err(e) => fail(e),
            },
            None => usage("sign-in takes only --no-wait"),
        },
        ["authorize-find-my", rest @ ..] => match no_wait(rest) {
            Some(true) => run(open_now(icloud_session::authorize_find_my)),
            Some(false) => match open_window(icloud_session::authorize_find_my) {
                Ok(s) if s.find_my_authorized => print(&status_json(&s)),
                Ok(s) => {
                    println!("{}", status_json(&s));
                    fail_with(
                        "find_my_auth_not_completed",
                        EXIT_FIND_MY_AUTH,
                        "Find My authorization did not complete",
                        None,
                    )
                }
                Err(e) => fail(e),
            },
            None => usage("authorize-find-my takes only --no-wait"),
        },
        ["set-password", rest @ ..] => match source(rest) {
            Some(source) => set_password(source),
            None => usage("set-password takes --from-bitwarden [ITEM] or --from-1password [ITEM]"),
        },
        ["forget-password"] => match call_daemon("ForgetPassword", &()) {
            Ok(()) => {
                eprintln!("icloud-session: removed the stored password from the keyring");
                run(icloud_session::status().map(|s| status_json(&s)))
            }
            Err(code) => code,
        },
        ["sign-out"] => run(icloud_session::sign_out()
            .and_then(|()| icloud_session::status())
            .map(|s| status_json(&s))),
        ["validate"] => run(validate()),
        ["-V" | "--version"] => {
            if JSON.load(Ordering::Relaxed) {
                println!("{}", json!({ "version": env!("CARGO_PKG_VERSION") }));
            } else {
                println!("icloud-session {}", env!("CARGO_PKG_VERSION"));
            }
            ExitCode::SUCCESS
        }
        [] => usage("no command given"),
        [name, ..] if COMMANDS.iter().any(|(n, ..)| n == name) => usage(&format!(
            "{name}: unexpected arguments (see icloud-session {name} --help)"
        )),
        [name, ..] => usage(&format!("unknown command \"{name}\"")),
    }
}

/// `--no-wait` or nothing; None for anything else.
fn no_wait(rest: &[&str]) -> Option<bool> {
    match rest {
        [] => Some(false),
        ["--no-wait"] => Some(true),
        _ => None,
    }
}

/// Opens the window (unless one is open) and returns the status at once.
fn open_now(open: fn() -> Result<(), Error>) -> Result<serde_json::Value, Error> {
    if !icloud_session::status()?.signing_in {
        open()?;
    }
    eprintln!("icloud-session: finish in the \"Sign in to iCloud\" window (it may be on another workspace)");
    icloud_session::status().map(|s| status_json(&s))
}

/// A usage error: the usage on stderr for a person, or the JSON error.
fn usage(message: &str) -> ExitCode {
    if !JSON.load(Ordering::Relaxed) {
        eprintln!("{}\n", usage_text());
    }
    fail_with("usage", EXIT_USAGE, message, None)
}

/// Reports an error on stderr (one JSON line with `--json`) and returns its
/// exit code.
fn fail_with(code: &str, exit: u8, message: &str, hint: Option<&str>) -> ExitCode {
    if JSON.load(Ordering::Relaxed) {
        let mut error = json!({ "code": code, "message": message, "exit_code": exit });
        if let Some(hint) = hint {
            error["hint"] = json!(hint);
        }
        eprintln!("{}", json!({ "error": error }));
    } else {
        eprintln!("icloud-session: {message}");
        if let Some(hint) = hint {
            eprintln!("{hint}");
        }
    }
    ExitCode::from(exit)
}

fn status_json(s: &Status) -> serde_json::Value {
    serde_json::to_value(s).expect("status serializes")
}

/// `SignIn()` or `AuthorizeFindMy()`, then waits for the window to close
/// (`SigningIn` false). A window already open is waited for instead.
fn open_window(open: fn() -> Result<(), Error>) -> Result<Status, Error> {
    let mut watch = icloud_session::watch()?;
    let before = icloud_session::status()?;
    if !before.signing_in {
        open()?;
    }
    eprintln!("icloud-session: finish in the \"Sign in to iCloud\" window (it may be on another workspace)");
    let mut opened = before.signing_in;
    if let Some(current) = watch.current()
        && current.signing_in
    {
        opened = true;
    }
    for status in watch.by_ref() {
        if status.signing_in {
            opened = true;
        } else if opened {
            return Ok(status);
        }
    }
    icloud_session::status()
}

fn validate() -> Result<serde_json::Value, Error> {
    let session = Session::connect()?;
    let webservices = session.webservices()?;
    Ok(json!({
        "dsid": session.dsid(),
        "apple_id": session.apple_id(),
        "webservices": webservices.urls,
    }))
}

fn run(result: Result<serde_json::Value, Error>) -> ExitCode {
    match result {
        Ok(value) => print(&value),
        Err(e) => fail(e),
    }
}

fn fail(e: Error) -> ExitCode {
    let message = e.to_string();
    match e {
        Error::SignInRequired => fail_with(
            "sign_in_required",
            EXIT_SIGN_IN,
            &message,
            Some("run `icloud-session sign-in` (a person signs in in the window)"),
        ),
        Error::FindMyAuthRequired => fail_with(
            "find_my_auth_required",
            EXIT_FIND_MY_AUTH,
            &message,
            Some("run `icloud-session authorize-find-my` (to stop being asked: `icloud-session set-password`)"),
        ),
        Error::Service(_) => fail_with("session_service", EXIT_ERROR, &message, None),
        Error::Network(_) => fail_with("network", EXIT_ERROR, &message, None),
        Error::Http { .. } => fail_with("http", EXIT_ERROR, &message, None),
        Error::Io(_) => fail_with("io", EXIT_ERROR, &message, None),
    }
}

fn print(value: &serde_json::Value) -> ExitCode {
    println!("{value}");
    ExitCode::SUCCESS
}

// ------------------------------------------------------------ set-password

/// Where `set-password` reads the password from.
enum Source<'a> {
    /// The terminal without echo, or stdin when it is not a terminal.
    Prompt,
    Bitwarden(Option<&'a str>),
    OnePassword(Option<&'a str>),
}

fn source<'a>(args: &[&'a str]) -> Option<Source<'a>> {
    match args {
        [] => Some(Source::Prompt),
        ["--from-bitwarden"] => Some(Source::Bitwarden(None)),
        ["--from-bitwarden", item] if !item.starts_with('-') => Some(Source::Bitwarden(Some(item))),
        ["--from-1password"] => Some(Source::OnePassword(None)),
        ["--from-1password", item] if !item.starts_with('-') => Some(Source::OnePassword(Some(item))),
        _ => None,
    }
}

type Secret = Zeroizing<String>;

fn set_password(source: Source<'_>) -> ExitCode {
    let status = match icloud_session::status() {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let Some(apple_id) = status.apple_id.filter(|_| status.signed_in) else {
        return fail_with(
            "sign_in_required",
            EXIT_SIGN_IN,
            "sign in first (icloud-session sign-in)",
            None,
        );
    };
    let password = match source {
        Source::Prompt => read_password(&apple_id),
        Source::Bitwarden(item) => from_manager(&BITWARDEN, item, &apple_id),
        Source::OnePassword(item) => from_manager(&ONE_PASSWORD, item, &apple_id),
    };
    let password = match password {
        Ok(p) if p.is_empty() => {
            return fail_with("error", EXIT_ERROR, "the password is empty; nothing stored", None);
        }
        Ok(p) => p,
        Err(message) => return fail_with("error", EXIT_ERROR, &message, None),
    };
    eprintln!("icloud-session: checking the password with a Find My sign-in…");
    // The password travels to the daemon in the D-Bus message only.
    let result = call_daemon("SetPassword", &(password.as_str(),));
    drop(password);
    match result {
        Ok(()) => {
            eprintln!(
                "icloud-session: Apple accepted the password; stored in the keyring for {apple_id}. \
                 Find My now re-authorizes itself (icloud-session forget-password to undo)"
            );
            run(icloud_session::status().map(|s| status_json(&s)))
        }
        Err(code) => code,
    }
}

/// From the terminal without echo, else all of stdin (one trailing newline
/// dropped).
fn read_password(apple_id: &str) -> Result<Secret, String> {
    if std::io::stdin().is_terminal() {
        let p = rpassword::prompt_password(format!("Apple ID password for {apple_id}: "))
            .map_err(|e| format!("reading the password: {e}"))?;
        return Ok(Zeroizing::new(p));
    }
    let mut p = Zeroizing::new(String::new());
    std::io::stdin()
        .read_to_string(&mut p)
        .map_err(|e| format!("reading the password from stdin: {e}"))?;
    Ok(strip_newline(p))
}

fn strip_newline(mut s: Secret) -> Secret {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

/// A password manager's CLI, read once.
struct Manager {
    name: &'static str,
    bin: &'static str,
    /// The arguments that print ITEM's password on stdout.
    args: fn(&str) -> Vec<String>,
    /// How to install it and unlock it.
    help: &'static str,
}

const BITWARDEN: Manager = Manager {
    name: "Bitwarden",
    bin: "bw",
    args: |item| ["--nointeraction", "get", "password", item].map(String::from).to_vec(),
    help: "install the Bitwarden CLI (sudo pacman -S bitwarden-cli), then bw login; \
           or unlock it yourself: export BW_SESSION=$(bw unlock --raw)",
};

const ONE_PASSWORD: Manager = Manager {
    name: "1Password",
    bin: "op",
    args: |item| {
        if item.starts_with("op://") {
            ["read", item].map(String::from).to_vec()
        } else {
            ["item", "get", item, "--fields", "label=password", "--reveal"]
                .map(String::from)
                .to_vec()
        }
    },
    help: "install 1Password and its CLI with omarchy-install-service-1password, then in the \
           1Password app turn on Settings → Developer → \"Integrate with 1Password CLI\" \
           (op then unlocks through the app)",
};

/// How the lookups reach an unlocked vault.
enum Unlock {
    /// Already unlocked (`BW_SESSION` exported, 1Password app integration).
    Ready,
    /// Unlocked by us: the key goes to the lookups (bw: `BW_SESSION` in the
    /// child's env; op: `--session`), and the vault is locked again after.
    Key(Secret),
}

/// What one lookup gave.
enum Lookup {
    Found(Secret),
    NotFound,
    /// More than one item matches (the manager listed them on stderr).
    Ambiguous,
    /// Locked, signed out, or not set up.
    Locked,
    Failed(String),
}

fn not_installed(manager: &Manager, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!("`{}` is not installed; {}", manager.bin, manager.help)
    } else {
        format!("running {}: {e}", manager.bin)
    }
}

/// Runs the manager with the terminal on stdin and stderr, so it asks for
/// what it needs itself, and returns what it printed (a session key).
fn interactive(manager: &Manager, args: &[&str]) -> Result<Secret, String> {
    let out = Command::new(manager.bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| not_installed(manager, e))?;
    let key = strip_newline(Zeroizing::new(String::from_utf8_lossy(&out.stdout).into_owned()));
    if !out.status.success() || key.is_empty() {
        // The manager is installed (it ran), so the usual cause is a
        // mistyped master password; its own error is already on screen.
        return Err(format!(
            "`{} {}` did not unlock the vault, usually a mistyped master password: run the command again. If it keeps failing: {}",
            manager.bin,
            args.join(" "),
            manager.help
        ));
    }
    Ok(key)
}

/// Unlocks Bitwarden unless `BW_SESSION` is exported: `bw login` when
/// signed out, else `bw unlock`, each asking on the terminal.
fn bitwarden_unlock() -> Result<Unlock, String> {
    if std::env::var_os("BW_SESSION").is_some_and(|v| !v.is_empty()) {
        return Ok(Unlock::Ready);
    }
    let out = Command::new(BITWARDEN.bin)
        .arg("status")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| not_installed(&BITWARDEN, e))?;
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let key = match status["status"].as_str() {
        Some("unauthenticated") => {
            eprintln!("icloud-session: signing in to Bitwarden (bw login)");
            interactive(&BITWARDEN, &["login", "--raw"])?
        }
        _ => {
            eprintln!("icloud-session: unlocking Bitwarden (bw unlock)");
            interactive(&BITWARDEN, &["unlock", "--raw"])?
        }
    };
    Ok(Unlock::Key(key))
}

/// Signs in to 1Password unless `op whoami` says it is (the desktop app's
/// CLI integration signs in by itself): `op signin`, asking on the terminal.
fn one_password_unlock() -> Result<Unlock, String> {
    let signed_in = Command::new(ONE_PASSWORD.bin)
        .arg("whoami")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| not_installed(&ONE_PASSWORD, e))?;
    if signed_in.success() {
        return Ok(Unlock::Ready);
    }
    eprintln!("icloud-session: signing in to 1Password (op signin)");
    Ok(Unlock::Key(interactive(&ONE_PASSWORD, &["signin", "--raw"])?))
}

/// Locks what [`bitwarden_unlock`] / [`one_password_unlock`] unlocked.
fn relock(manager: &Manager, unlock: &Unlock) {
    let Unlock::Key(key) = unlock else { return };
    let mut cmd = Command::new(manager.bin);
    if manager.bin == BITWARDEN.bin {
        cmd.arg("lock").env("BW_SESSION", key.as_str());
    } else {
        cmd.args(["signout", "--session", key.as_str()]);
    }
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status();
}

/// The password from `manager`: ITEM, or with none the login saved for
/// apple.com or icloud.com (see [`find_apple_login`]). Unlocks the vault
/// first if needed, and locks it again after if it did.
fn from_manager(manager: &Manager, item: Option<&str>, apple_id: &str) -> Result<Secret, String> {
    let unlock = if manager.bin == BITWARDEN.bin {
        bitwarden_unlock()
    } else {
        one_password_unlock()
    }
    .inspect_err(|_| {
        // A failed unlock leaves nothing to lock, but lock anyway.
        if manager.bin == BITWARDEN.bin && std::env::var_os("BW_SESSION").is_none() {
            let _ = Command::new(BITWARDEN.bin)
                .arg("lock")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    })?;
    let result = find_item(manager, item, apple_id, &unlock);
    relock(manager, &unlock);
    result
}

fn find_item(manager: &Manager, item: Option<&str>, apple_id: &str, unlock: &Unlock) -> Result<Secret, String> {
    let Some(item) = item else {
        return find_apple_login(manager, apple_id, unlock);
    };
    let candidates = [item];
    for candidate in &candidates {
        match lookup(manager, candidate, unlock)? {
            Lookup::Found(p) => {
                eprintln!(
                    "icloud-session: read the password of \"{candidate}\" from {}",
                    manager.name
                );
                return Ok(p);
            }
            Lookup::NotFound => continue,
            Lookup::Ambiguous => {
                return Err(format!(
                    "more than one {} item matches \"{candidate}\" (listed above); \
                     pass the one to use as ITEM",
                    manager.name
                ));
            }
            Lookup::Locked => {
                return Err(format!("{} is locked or not signed in; {}", manager.name, manager.help));
            }
            Lookup::Failed(m) => return Err(format!("{}: {m}", manager.name)),
        }
    }
    let tried = candidates
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "no {} item with a password found (tried {tried}); pass ITEM",
        manager.name
    ))
}

/// One login a password manager holds for an Apple site.
struct AppleLogin {
    /// What to fetch the password by (`bw`/`op` item id).
    id: String,
    name: String,
    username: String,
    site: String,
}

/// Whether a saved URL is one of Apple's sign-in sites.
fn is_apple_site(uri: &str) -> bool {
    let with_scheme = if uri.contains("://") {
        uri.to_string()
    } else {
        format!("https://{uri}")
    };
    url::Url::parse(&with_scheme)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|h| {
            ["apple.com", "icloud.com"]
                .iter()
                .any(|d| h == *d || h.ends_with(&format!(".{d}")))
        })
}

/// With no ITEM: the login saved for apple.com or icloud.com, as a browser
/// extension would pick it. Several: those for the Apple ID first; still
/// several, the user picks one by number (names and sites only are shown).
fn find_apple_login(manager: &Manager, apple_id: &str, unlock: &Unlock) -> Result<Secret, String> {
    let mut logins = list_apple_logins(manager, unlock)?;
    if logins.is_empty() {
        return Err(format!(
            "no {} login saved for apple.com or icloud.com; pass the item to use as ITEM",
            manager.name
        ));
    }
    let mine: Vec<usize> = (0..logins.len())
        .filter(|&i| logins[i].username.eq_ignore_ascii_case(apple_id))
        .collect();
    if !mine.is_empty() {
        let mut i = 0;
        logins.retain(|_| {
            let keep = mine.contains(&i);
            i += 1;
            keep
        });
    }
    let chosen = if logins.len() == 1 {
        logins.remove(0)
    } else {
        eprintln!("icloud-session: {} has several Apple logins:", manager.name);
        for (n, l) in logins.iter().enumerate() {
            eprintln!("  {}) {} — {} — {}", n + 1, l.name, l.username, l.site);
        }
        if !std::io::stdin().is_terminal() {
            return Err("several Apple logins match; pass the one to use as ITEM".into());
        }
        eprint!("Use which one? [1-{}] ", logins.len());
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
        let n: usize = answer.trim().parse().map_err(|_| "no login chosen".to_string())?;
        if n == 0 || n > logins.len() {
            return Err("no login chosen".into());
        }
        logins.remove(n - 1)
    };
    eprintln!(
        "icloud-session: using {}'s \"{}\" ({})",
        manager.name, chosen.name, chosen.username
    );
    match lookup(manager, &chosen.id, unlock)? {
        Lookup::Found(p) => Ok(p),
        Lookup::Locked => Err(format!("{} is locked or not signed in; {}", manager.name, manager.help)),
        _ => Err(format!("{} gave no password for \"{}\"", manager.name, chosen.name)),
    }
}

/// Every login the manager holds for an Apple site: names, usernames and
/// sites only; passwords are fetched for the chosen one alone.
fn list_apple_logins(manager: &Manager, unlock: &Unlock) -> Result<Vec<AppleLogin>, String> {
    let bitwarden = manager.bin == BITWARDEN.bin;
    let mut found: Vec<AppleLogin> = Vec::new();
    let searches: &[&[&str]] = if bitwarden {
        &[
            &["--nointeraction", "list", "items", "--search", "apple.com"],
            &["--nointeraction", "list", "items", "--search", "icloud.com"],
        ]
    } else {
        &[&["item", "list", "--categories", "Login", "--format", "json"]]
    };
    for args in searches {
        let mut cmd = Command::new(manager.bin);
        cmd.args(*args);
        if let Unlock::Key(key) = unlock {
            if bitwarden {
                cmd.env("BW_SESSION", key.as_str());
            } else {
                cmd.args(["--session", key.as_str()]);
            }
        }
        let output = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| not_installed(manager, e))?;
        // bw's list output carries passwords: keep it zeroized, parse, drop.
        let stdout = Zeroizing::new(output.stdout);
        if !output.status.success() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            return Err(format!("{} could not list logins; {}", manager.name, manager.help));
        }
        let items: Vec<serde_json::Value> =
            serde_json::from_slice(&stdout).map_err(|e| format!("{} printed unexpected JSON: {e}", manager.bin))?;
        for item in &items {
            let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
            let (id, name, username, uris): (String, String, String, Vec<String>) = if bitwarden {
                if item["type"].as_i64() != Some(1) {
                    continue;
                }
                let uris = item["login"]["uris"]
                    .as_array()
                    .map(|a| a.iter().map(|u| text(&u["uri"])).collect())
                    .unwrap_or_default();
                (
                    text(&item["id"]),
                    text(&item["name"]),
                    text(&item["login"]["username"]),
                    uris,
                )
            } else {
                let uris = item["urls"]
                    .as_array()
                    .map(|a| a.iter().map(|u| text(&u["href"])).collect())
                    .unwrap_or_default();
                (
                    text(&item["id"]),
                    text(&item["title"]),
                    text(&item["additional_information"]),
                    uris,
                )
            };
            let Some(site) = uris.into_iter().find(|u| is_apple_site(u)) else {
                continue;
            };
            if !id.is_empty() && !found.iter().any(|l| l.id == id) {
                found.push(AppleLogin {
                    id,
                    name,
                    username,
                    site,
                });
            }
        }
    }
    Ok(found)
}

/// Runs the manager's CLI for one item. The password comes back only on
/// the child's stdout pipe; its stderr is passed on to the user (and read
/// to tell "not found" from "locked").
fn lookup(manager: &Manager, item: &str, unlock: &Unlock) -> Result<Lookup, String> {
    let mut cmd = Command::new(manager.bin);
    cmd.args((manager.args)(item));
    if let Unlock::Key(key) = unlock {
        if manager.bin == BITWARDEN.bin {
            cmd.env("BW_SESSION", key.as_str());
        } else {
            cmd.args(["--session", key.as_str()]);
        }
    }
    let output = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| not_installed(manager, e))?;
    let stdout = Zeroizing::new(output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprint!("{stderr}");
    let said = stderr.to_lowercase();
    if output.status.success() {
        let text = std::str::from_utf8(&stdout).map_err(|_| format!("{} printed a non-UTF-8 password", manager.bin))?;
        let password = strip_newline(Zeroizing::new(text.to_string()));
        return Ok(if password.is_empty() {
            Lookup::NotFound
        } else {
            Lookup::Found(password)
        });
    }
    Ok(if said.contains("more than one") {
        Lookup::Ambiguous
    } else if ["not found", "isn't an item", "no item"]
        .iter()
        .any(|s| said.contains(s))
    {
        Lookup::NotFound
    } else if [
        "locked",
        "not logged in",
        "not signed in",
        "sign in",
        "signin",
        "no accounts",
        "session",
        "unlock",
        "authoriz",
    ]
    .iter()
    .any(|s| said.contains(s))
    {
        Lookup::Locked
    } else {
        Lookup::Failed(format!("{} exited with {}", manager.bin, output.status))
    })
}

/// Calls a daemon method that answers nothing, mapping its errors to a
/// message and an exit code.
fn call_daemon<B>(method: &str, body: &B) -> Result<(), ExitCode>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let conn = zbus::blocking::Connection::session().map_err(|e| fail(Error::from(e)))?;
    match conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, body) {
        Ok(_) => Ok(()),
        Err(zbus::Error::MethodError(name, message, _)) => {
            let message = message.unwrap_or_else(|| name.to_string());
            Err(match name.as_str().rsplit('.').next() {
                Some("SignInRequired") => fail_with(
                    "sign_in_required",
                    EXIT_SIGN_IN,
                    "sign in first (icloud-session sign-in)",
                    None,
                ),
                _ => fail_with("error", EXIT_ERROR, &message, None),
            })
        }
        Err(e) => Err(fail(Error::from(e))),
    }
}
