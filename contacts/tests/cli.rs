//! The built binary's command line against the fake iCloud Contacts server
//! (examples/fake_contacts.rs) through `icloud-session`'s mock mode: no
//! D-Bus, no Apple account, a temporary data directory.

#[path = "../examples/fake_contacts.rs"]
mod fake;

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use serde_json::{Value, json};

struct Env {
    base: String,
    state: Arc<fake::State>,
    dir: tempfile::TempDir,
}

/// A seeded fake server of its own per test.
fn start() -> Env {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(fake::State::seeded());
    let s = state.clone();
    std::thread::spawn(move || fake::serve_with(listener, s));
    Env {
        base,
        state,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Env {
    fn run(&self, args: &[&str]) -> Output {
        run_at(&self.base, self.dir.path(), args)
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        stdout(&out)
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_str(&self.ok(&all)).unwrap()
    }

    /// The card writes sent: `(method, body)`, method "" for a create.
    fn writes(&self) -> Vec<(String, Value)> {
        self.state
            .requests()
            .into_iter()
            .filter(|r| r["path"] == "/co/contacts/card/")
            .map(|r| {
                (
                    r["query"]["method"].as_str().unwrap_or("").to_owned(),
                    r["body"].clone(),
                )
            })
            .collect()
    }
}

/// Stdin not a terminal, `--data-dir <tmp>/data`, a home and XDG
/// directories in the temp dir, and no session bus.
fn run_at(base: &str, tmp: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_icloud-contacts"))
        .args(args)
        .arg("--data-dir")
        .arg(tmp.join("data"))
        .env("ICLOUD_SESSION_MOCK", "1")
        .env("ICLOUD_SESSION_MOCK_URL", base)
        .env("HOME", tmp.join("home"))
        .env("XDG_DATA_HOME", tmp.join("xdg"))
        .env("FAKE_CONTACTS_QUIET", "1")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn write_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn error_code(out: &Output) -> String {
    let line = stderr(out);
    let v: Value = serde_json::from_str(line.lines().last().unwrap()).unwrap();
    v["error"]["code"].as_str().unwrap().to_owned()
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn lists_sorted_by_last_name_and_searches() {
    let env = start();
    assert_eq!(
        names(&env.json(&["list"])),
        ["Acme Hardware", "Bob Berg", "Ann Lee", "Mock User"]
    );
    assert_eq!(names(&env.json(&["list", "acme"])), ["Acme Hardware", "Ann Lee"]);
    // Digits match phone numbers whatever their punctuation.
    assert_eq!(names(&env.json(&["list", "010 0100"])), ["Ann Lee"]);
    let text = env.ok(&["list", "--cached"]);
    assert!(
        text.contains("Ann Lee  (Acme, +1 (555) 010-0100, ann@acme.example)"),
        "{text}"
    );
}

#[test]
fn shows_a_contact_by_name_phone_email_or_me() {
    let env = start();
    let ann = env.json(&["show", "ann lee"]);
    assert_eq!(ann["id"], "ANN-LEE");
    assert_eq!(
        ann["phones"][0],
        json!({ "label": "mobile", "value": "+1 (555) 010-0100" })
    );
    assert_eq!(ann["addresses"][0]["city"], "Springfield");
    assert_eq!(ann["birthday"], "1990-05-01");
    assert_eq!(ann["has_photo"], true);
    assert_eq!(env.json(&["show", "5550100100", "--cached"])["id"], "ANN-LEE");
    assert_eq!(env.json(&["show", "ANN@acme.example", "--cached"])["id"], "ANN-LEE");
    let me = env.json(&["show", "me", "--cached"]);
    assert_eq!((me["id"].as_str(), me["me"].as_bool()), (Some("ME-CARD"), Some(true)));
    let text = env.ok(&["show", "bob", "--cached"]);
    assert!(
        text.contains("phone:     +47 555 01 234  (iPhone)") && text.contains("Met at the conference"),
        "{text}"
    );
}

#[test]
fn not_found_and_ambiguous() {
    let env = start();
    let out = env.run(&["--json", "show", "zed"]);
    assert_eq!((code(&out), error_code(&out)), (1, "not_found".into()));
    // "a" is in several names.
    let out = env.run(&["--json", "show", "a", "--cached"]);
    assert_eq!(error_code(&out), "ambiguous");
}

#[test]
fn add_finds_its_card_by_id_in_a_changeset_of_others() {
    let env = start();
    env.ok(&["sync"]);
    // Another device changes Bob after this computer's tokens were issued:
    // the add's answer then lists Bob first.
    env.state
        .put(json!({ "contactId": "BOB-BERG", "firstName": "Bob", "lastName": "Berg", "nickName": "B" }));
    let bob_before = env.state.card("BOB-BERG").unwrap();
    let added = env.json(&[
        "add",
        "--cached",
        "--first",
        "Cleo",
        "--last",
        "Park",
        "--phone",
        "work=+44 20 7946 0000",
        "--email",
        "cleo@example.org",
        "--birthday",
        "07-04",
    ]);
    let c = &added["contact"];
    assert_eq!(c["name"], "Cleo Park");
    assert_eq!(c["phones"][0], json!({ "label": "work", "value": "+44 20 7946 0000" }));
    assert_eq!(c["emails"][0]["label"], "home");
    assert_eq!(c["birthday"], "07-04");
    let id = c["id"].as_str().unwrap();
    assert_eq!(env.state.card(id).unwrap()["birthday"], "1604-07-04");
    // Only the create was written; Bob is as the other device left him.
    assert_eq!(env.writes().len(), 1);
    assert_eq!(env.state.card("BOB-BERG").unwrap(), bob_before);
    // The cache has the new card, and Bob was not overwritten with it.
    assert_eq!(env.json(&["show", "cleo", "--cached"])["id"], id);
    assert_eq!(env.json(&["show", "bob", "--cached"])["name"], "Bob Berg");
}

#[test]
fn add_needs_a_name_phone_or_email() {
    let env = start();
    let out = env.run(&["--json", "add", "--notes", "who?"]);
    assert_eq!((code(&out), error_code(&out)), (64, "usage".into()));
    let out = env.run(&["--json", "add", "--phone", "call me"]);
    assert_eq!(code(&out), 64);
    let company = env.json(&["add", "--company", "Globex", "--is-company"]);
    assert_eq!(
        (
            company["contact"]["name"].as_str(),
            company["contact"]["is_company"].as_bool()
        ),
        (Some("Globex"), Some(true))
    );
}

#[test]
fn edit_changes_only_what_it_names() {
    let env = start();
    let out = env.json(&[
        "edit",
        "ann",
        "--job-title",
        "Manager",
        "--nickname",
        "",
        "--phone",
        "home=+1 555 010 0177",
        "--remove-phone",
        "555-010-0199",
        "--remove-email",
        "ANN@ACME.EXAMPLE",
    ]);
    let c = &out["contact"];
    assert_eq!(c["job_title"], "Manager");
    let phones: Vec<&str> = c["phones"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["value"].as_str().unwrap())
        .collect();
    assert_eq!(phones, ["+1 (555) 010-0100", "+1 555 010 0177"]);
    assert_eq!(c["emails"], json!([]));
    // What this tool does not show went back as it was.
    let card = env.state.card("ANN-LEE").unwrap();
    assert_eq!(card["photo"]["signature"], "sig");
    assert_eq!(card["profiles"][0]["user"], "ann");
    assert_eq!(card["streetAddresses"][0]["field"]["city"], "Springfield");
    assert!(card.get("emailAddresses").is_none());
    let (method, body) = &env.writes()[0];
    assert_eq!(method, "PUT");
    assert_eq!(body["contacts"][0]["contactId"], "ANN-LEE");

    let out = env.run(&["--json", "edit", "ann", "--remove-phone", "999 999", "--cached"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(stderr(&out).contains("has no phone number 999 999"));
    let out = env.run(&["--json", "edit", "ann", "--cached"]);
    assert_eq!(code(&out), 64);
}

#[test]
fn an_edit_over_a_change_from_another_device_keeps_both() {
    let env = start();
    env.ok(&["sync"]);
    let mut theirs = env.state.card("ANN-LEE").unwrap();
    theirs["jobTitle"] = json!("Director");
    env.state.put(theirs);
    // The cached etag is now stale: iCloud refuses with 412, and the edit is
    // made again over the fresh card.
    let out = env.json(&["edit", "ann", "--cached", "--last", "Lee-Smith"]);
    assert_eq!(out["contact"]["last"], "Lee-Smith");
    assert_eq!(out["contact"]["job_title"], "Director");
    let puts: Vec<_> = env.writes().into_iter().filter(|(m, _)| m == "PUT").collect();
    assert_eq!(puts.len(), 2);
}

#[test]
fn expired_tokens_are_renewed_once() {
    let env = start();
    env.ok(&["sync"]);
    env.state.expire_tokens();
    let out = env.json(&["edit", "bob", "--cached", "--company", "Fjord AS"]);
    assert_eq!(out["contact"]["company"], "Fjord AS");
    let startups = env
        .state
        .requests()
        .iter()
        .filter(|r| r["path"] == "/co/startup")
        .count();
    assert_eq!(startups, 2, "the sync's, then one for fresh tokens");
}

#[test]
fn delete_asks_or_needs_yes() {
    let env = start();
    let out = env.run(&["--json", "delete", "bob"]);
    assert_eq!((code(&out), error_code(&out)), (64, "usage".into()));
    assert!(env.state.card("BOB-BERG").is_some());
    let out = env.json(&["delete", "bob", "--yes"]);
    assert_eq!(out["contact"], json!({ "id": "BOB-BERG", "name": "Bob Berg" }));
    assert!(env.state.card("BOB-BERG").is_none());
    assert_eq!(env.state.count(), 3);
    let out = env.run(&["--json", "show", "bob", "--cached"]);
    assert_eq!(error_code(&out), "not_found");
}

#[test]
fn exports_vcards() {
    let env = start();
    let text = env.ok(&["export"]);
    assert_eq!(text.matches("BEGIN:VCARD\r\n").count(), 4);
    assert!(
        text.contains("FN:Ann Lee\r\n") && text.contains("TEL;TYPE=CELL:+47 555 01 234\r\n"),
        "{text}"
    );
    let file = env.dir.path().join("ann.vcf");
    let out = env.json(&["export", "ann", "--cached", "--output", file.to_str().unwrap()]);
    assert_eq!(out["count"], 1);
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(
        written.contains("BDAY:1990-05-01\r\n") && written.contains("UID:ANN-LEE\r\n"),
        "{written}"
    );
}

#[test]
fn signed_out_is_exit_2() {
    let env = start();
    *env.state.signed_out.lock().unwrap() = true;
    let out = env.run(&["--json", "list"]);
    assert_eq!((code(&out), error_code(&out)), (2, "sign_in_required".into()));
}

#[test]
fn sync_counts_changes() {
    let env = start();
    assert_eq!(env.json(&["sync"]), json!({ "contacts": 4, "changed": 4 }));
    assert_eq!(env.json(&["sync"]), json!({ "contacts": 4, "changed": 0 }));
    env.state.put(json!({ "contactId": "NEW", "firstName": "Dee" }));
    assert_eq!(env.json(&["sync"]), json!({ "contacts": 5, "changed": 1 }));
}

/// The shared libraries a 64-bit little-endian ELF executable needs (its
/// `DT_NEEDED` entries).
fn needed_libs(path: &str) -> Vec<String> {
    let f = std::fs::read(path).unwrap();
    let u16_at = |o: usize| u16::from_le_bytes(f[o..o + 2].try_into().unwrap()) as usize;
    let u64_at = |o: usize| u64::from_le_bytes(f[o..o + 8].try_into().unwrap()) as usize;
    assert_eq!(&f[..5], b"\x7fELF\x02", "a 64-bit ELF file");
    let (phoff, phentsize, phnum) = (u64_at(0x20), u16_at(0x36), u16_at(0x38));
    let headers: Vec<usize> = (0..phnum).map(|i| phoff + i * phentsize).collect();
    let p_type = |h: usize| u32::from_le_bytes(f[h..h + 4].try_into().unwrap());
    let file_offset = |vaddr: usize| {
        headers
            .iter()
            .filter(|&&h| p_type(h) == 1)
            .find(|&&h| (u64_at(h + 0x10)..u64_at(h + 0x10) + u64_at(h + 0x20)).contains(&vaddr))
            .map(|&h| vaddr - u64_at(h + 0x10) + u64_at(h + 0x08))
            .expect("address in a loaded segment")
    };
    let dynamic = headers.iter().find(|&&h| p_type(h) == 2).expect("dynamically linked");
    let entries: Vec<(usize, usize)> = (u64_at(dynamic + 0x08)..u64_at(dynamic + 0x08) + u64_at(dynamic + 0x20))
        .step_by(16)
        .map(|e| (u64_at(e), u64_at(e + 8)))
        .take_while(|&(tag, _)| tag != 0)
        .collect();
    let strtab = file_offset(entries.iter().find(|e| e.0 == 5).expect("DT_STRTAB").1);
    entries
        .iter()
        .filter(|e| e.0 == 1)
        .map(|&(_, name)| {
            let start = strtab + name;
            let end = start + f[start..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8_lossy(&f[start..end]).into_owned()
        })
        .collect()
}

/// `icloud-contacts` is what agents and scripts run: it must not load GTK.
#[test]
fn the_command_line_binary_does_not_link_gtk() {
    let ui = |libs: Vec<String>| {
        libs.into_iter()
            .filter(|l| ["libgtk-4.", "libadwaita-1."].iter().any(|p| l.starts_with(p)))
            .count()
    };
    let cli = needed_libs(env!("CARGO_BIN_EXE_icloud-contacts"));
    assert!(cli.iter().any(|l| l.starts_with("libc.")), "{cli:?}");
    assert_eq!(ui(cli.clone()), 0, "{cli:?}");
    // Named but not built without the `ui` feature.
    if let Some(app) = option_env!("CARGO_BIN_EXE_icloud-contacts-app").filter(|p| Path::new(p).exists()) {
        assert_eq!(ui(needed_libs(app)), 2);
    }
}

/// With no command, `icloud-contacts` becomes `icloud-contacts-app` from
/// its own directory (the same process: its exit code is the app's).
#[test]
fn no_command_runs_the_app_beside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("icloud-contacts");
    std::fs::copy(env!("CARGO_BIN_EXE_icloud-contacts"), &cli).unwrap();
    let marker = tmp.path().join("ran");
    write_script(
        &tmp.path().join("icloud-contacts-app"),
        &format!("#!/bin/sh\necho \"$#\" > '{}'\nexit 7\n", marker.display()),
    );
    // A test thread forking while the copy was open for writing holds its
    // fd until that child execs: "Text file busy" (ETXTBSY) for a moment.
    let mut tries = 0;
    let out = loop {
        match Command::new(&cli).stdin(Stdio::null()).output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            r => break r.unwrap(),
        }
    };
    assert_eq!(code(&out), 7, "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "0");
}
