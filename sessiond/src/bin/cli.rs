//! `icloud-session status | sign-in | authorize-find-my | set-password |
//! forget-password | sign-out | validate`
//!
//! Talks to icloud-sessiond over D-Bus. JSON on stdout; errors on stderr
//! with a non-zero exit, 2 meaning "sign in to iCloud required", 3 "Find My
//! not authorized".

use std::io::{IsTerminal, Read};
use std::process::{Command, ExitCode, Stdio};

use icloud_session::{BUS_NAME, Error, INTERFACE, OBJECT_PATH, Session, Status};
use serde_json::json;
use zeroize::Zeroizing;

const USAGE: &str = "usage: icloud-session <command>

commands:
  status     the daemon's properties: signed_in, apple_id, dsid, expires_at, signing_in,
             find_my_authorized, find_my_password_stored
  sign-in    open the sign-in window and wait until it closes, then print status
  authorize-find-my
             open the sign-in window on Find My, where Apple asks for the password
             again, and wait until it closes, then print status
  set-password [--from-bitwarden [ITEM] | --from-1password [ITEM]]
             store the Apple ID password in the keyring (GNOME Keyring), so the daemon
             re-authorizes Find My by itself when it asks for the password again.
             The password is checked with a Find My sign-in first and stored only if
             Apple accepts it. It is read from the terminal without echo, from stdin
             when that is not a terminal, or once from Bitwarden (`bw get password`)
             or 1Password (`op`). Without ITEM: the Apple ID, then \"Apple ID\",
             \"Apple\", \"iCloud\"; with 1Password, ITEM may be an op:// reference
  forget-password
             remove the stored password from the keyring
  sign-out   forget the account and the sign-in window's profile, then print status
             (a stored password stays until forget-password)
  validate   the session's webservices (the daemon revalidates when older than 10 minutes)

JSON goes to stdout. Exit codes: 0 ok, 1 error, 2 sign-in required, 3 Find My not
authorized, 64 usage.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["status"] => run(icloud_session::status().map(|s| status_json(&s))),
        ["sign-in"] => match open_window(icloud_session::sign_in) {
            Ok(s) if s.signed_in => print(&status_json(&s)),
            Ok(s) => {
                println!("{}", status_json(&s));
                eprintln!("icloud-session: sign-in did not complete");
                ExitCode::from(2)
            }
            Err(e) => fail(e),
        },
        ["authorize-find-my"] => match open_window(icloud_session::authorize_find_my) {
            Ok(s) if s.find_my_authorized => print(&status_json(&s)),
            Ok(s) => {
                println!("{}", status_json(&s));
                eprintln!("icloud-session: Find My authorization did not complete");
                ExitCode::from(3)
            }
            Err(e) => fail(e),
        },
        ["set-password", rest @ ..] => match source(rest) {
            Some(source) => set_password(source),
            None => {
                eprintln!("{USAGE}");
                ExitCode::from(64)
            }
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
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["-V" | "--version"] => {
            println!("icloud-session {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
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
    eprintln!("icloud-session: {e}");
    match e {
        Error::SignInRequired => ExitCode::from(2),
        Error::FindMyAuthRequired => ExitCode::from(3),
        _ => ExitCode::FAILURE,
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
        eprintln!("icloud-session: sign in first (icloud-session sign-in)");
        return ExitCode::from(2);
    };
    let password = match source {
        Source::Prompt => read_password(&apple_id),
        Source::Bitwarden(item) => from_manager(&BITWARDEN, item, &apple_id),
        Source::OnePassword(item) => from_manager(&ONE_PASSWORD, item, &apple_id),
    };
    let password = match password {
        Ok(p) if p.is_empty() => {
            eprintln!("icloud-session: the password is empty; nothing stored");
            return ExitCode::FAILURE;
        }
        Ok(p) => p,
        Err(message) => {
            eprintln!("icloud-session: {message}");
            return ExitCode::FAILURE;
        }
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

/// The password from `manager`: ITEM, or the first of the default names
/// that names exactly one item with a password. Unlocks the vault first if
/// needed, and locks it again after if it did.
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
    let defaults = [apple_id, "Apple ID", "Apple", "iCloud"];
    let candidates: Vec<&str> = match item {
        Some(item) => vec![item],
        None => defaults.to_vec(),
    };
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
                Some("SignInRequired") => {
                    eprintln!("icloud-session: sign in first (icloud-session sign-in)");
                    ExitCode::from(2)
                }
                _ => {
                    eprintln!("icloud-session: {message}");
                    ExitCode::FAILURE
                }
            })
        }
        Err(e) => Err(fail(Error::from(e))),
    }
}
