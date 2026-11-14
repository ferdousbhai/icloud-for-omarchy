//! The built binary's command line against the fake iCloud Contacts server
//! (examples/fake_contacts.rs) through `icloud-session`'s mock mode: no
//! D-Bus, no Apple account, a temporary data directory.

#[path = "../examples/fake_contacts.rs"]
mod fake;

use std::net::TcpListener;
use std::process::Output;
use std::sync::Arc;

use icloud_app::testing::{
    assert_cli_links_no_gtk, assert_no_command_runs_the_app, code, command, envelope, stderr, stdout,
};
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
    /// Stdin not a terminal, `--data-dir <tmp>/data`, a home and XDG
    /// directories in the temp dir, and no session bus.
    fn run(&self, args: &[&str]) -> Output {
        command(env!("CARGO_BIN_EXE_icloud-contacts"), &self.base, self.dir.path())
            .args(args)
            .env("FAKE_CONTACTS_QUIET", "1")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        stdout(&out)
    }

    /// The `data` of the JSON envelope on stdout (the default when piped).
    fn json(&self, args: &[&str]) -> Value {
        let v = envelope(self.ok(args));
        assert_eq!(v["ok"], true, "{v}");
        v["data"].clone()
    }

    /// Human-readable text: `--styled` forces it even when piped.
    fn styled(&self, args: &[&str]) -> String {
        let mut all = args.to_vec();
        all.push("--styled");
        self.ok(&all)
    }

    /// A failing run: its exit code and the error object of the envelope.
    fn fails(&self, args: &[&str]) -> (i32, Value) {
        let out = self.run(args);
        let v = envelope(&out.stdout);
        assert_eq!(v["ok"], false, "{args:?}: {v}");
        (code(&out), v["error"].clone())
    }

    /// A failing run: its exit code and error code.
    fn error(&self, args: &[&str]) -> (i32, String) {
        let (exit, e) = self.fails(args);
        (exit, e["code"].as_str().unwrap().to_owned())
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
    let text = env.styled(&["list", "--cached"]);
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
    let text = env.styled(&["show", "bob", "--cached"]);
    assert!(
        text.contains("phone:     +47 555 01 234  (iPhone)") && text.contains("Met at the conference"),
        "{text}"
    );
}

#[test]
fn not_found_and_ambiguous() {
    let env = start();
    let (exit, e) = env.fails(&["show", "zed"]);
    assert_eq!((exit, e["code"].as_str()), (4, Some("not_found")));
    assert!(e["hint"].as_str().unwrap().contains("icloud-contacts list"), "{e}");
    // "a" is in several names: the hint lists them with their IDs.
    let (exit, e) = env.fails(&["show", "a", "--cached"]);
    assert_eq!((exit, e["code"].as_str()), (2, Some("ambiguous")));
    let hint = e["hint"].as_str().unwrap();
    assert!(
        hint.contains("Ann Lee  (ANN-LEE)") && hint.contains("(ACME-CO)"),
        "{hint}"
    );
    // The same for a command that changes something, and nothing was sent.
    assert_eq!(env.error(&["edit", "zed", "--last", "Z", "--cached"]).0, 4);
    assert_eq!(env.error(&["delete", "a", "--yes", "--cached"]).0, 2);
    assert!(env.writes().is_empty());
}

/// In text mode a failure is `error:`/`hint:` lines on stderr and nothing on
/// stdout; in every machine mode it is the error object on stdout.
#[test]
fn errors_go_to_stderr_as_text_and_to_stdout_as_json() {
    let env = start();
    let out = env.run(&["show", "zed", "--styled"]);
    assert_eq!((code(&out), stdout(&out).as_str()), (4, ""));
    let err = stderr(&out);
    assert!(err.starts_with("error: no contact matches \"zed\"\nhint: "), "{err}");
    for flag in ["--json", "--quiet", "--ids-only", "--count"] {
        let out = env.run(&["show", "zed", "--cached", flag]);
        let v = envelope(&out.stdout);
        assert_eq!((code(&out), v["ok"].as_bool()), (4, Some(false)), "{flag}");
        assert_eq!(v["error"]["code"], "not_found", "{flag}");
        assert_eq!(stderr(&out), "", "{flag}");
    }
    // A usage error clap finds reads the same way.
    let (exit, e) = env.fails(&["list", "--no-such-flag"]);
    assert_eq!((exit, e["code"].as_str()), (2, Some("usage")));
    assert_eq!(e["hint"], "run `icloud-contacts --help`");
}

#[test]
fn the_envelope_ids_and_counts() {
    let env = start();
    let v = envelope(env.ok(&["list"]));
    assert_eq!(
        (v["ok"].as_bool(), v["summary"].as_str()),
        (Some(true), Some("4 contacts"))
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 4);
    // `--json` is the same when piped; `--quiet` is only the data.
    assert_eq!(envelope(env.ok(&["list", "--cached", "--json"])), v);
    assert_eq!(envelope(env.ok(&["list", "--cached", "--quiet"])), v["data"]);
    assert_eq!(
        env.ok(&["list", "--cached", "--ids-only"]),
        "ACME-CO\nBOB-BERG\nANN-LEE\nME-CARD\n"
    );
    assert_eq!(
        env.ok(&["list", "acme", "--cached", "--ids-only"]),
        "ACME-CO\nANN-LEE\n"
    );
    assert_eq!(env.ok(&["list", "--cached", "--count"]), "4\n");
    assert_eq!(env.ok(&["list", "acme", "--cached", "--count"]), "2\n");
    assert_eq!(env.ok(&["list", "nobody", "--cached", "--count"]), "0\n");
    assert_eq!(env.ok(&["list", "nobody", "--cached", "--ids-only"]), "");
    assert_eq!(env.styled(&["list", "nobody", "--cached"]), "No contacts.\n");
    // ids-only wins over count, quiet and json.
    assert_eq!(
        env.ok(&["show", "ann", "--cached", "--json", "--count", "--ids-only"]),
        "ANN-LEE\n"
    );
    assert_eq!(env.ok(&["show", "ann", "--cached", "--count"]), "1\n");
    assert_eq!(env.ok(&["export", "ann", "bob", "--cached", "--count"]), "2\n");
    assert_eq!(
        env.ok(&["export", "ann", "bob", "--cached", "--ids-only"]),
        "ANN-LEE\nBOB-BERG\n"
    );
    // The ids of what a command changed.
    assert_eq!(
        env.ok(&["edit", "bob", "--cached", "--nickname", "B", "--ids-only"]),
        "BOB-BERG\n"
    );
    assert_eq!(
        env.ok(&["delete", "bob", "--cached", "--yes", "--ids-only"]),
        "BOB-BERG\n"
    );
    let id = env.ok(&["add", "--first", "Dee", "--ids-only"]);
    assert_eq!(env.json(&["show", id.trim()])["name"], "Dee");
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
    assert_eq!(env.error(&["add", "--notes", "who?"]), (2, "usage".into()));
    assert_eq!(env.error(&["add", "--phone", "call me"]), (2, "usage".into()));
    assert!(env.writes().is_empty());
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

    let (exit, e) = env.fails(&["edit", "ann", "--remove-phone", "999 999", "--cached"]);
    assert_eq!((exit, e["code"].as_str()), (1, Some("error")));
    assert!(
        e["message"].as_str().unwrap().contains("has no phone number 999 999"),
        "{e}"
    );
    assert_eq!(env.error(&["edit", "ann", "--cached"]), (2, "usage".into()));
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
    // Piped: nobody can be asked, in a machine mode or as text.
    let (exit, e) = env.fails(&["delete", "bob"]);
    assert_eq!((exit, e["code"].as_str()), (2, Some("usage")));
    assert_eq!(e["hint"], "pass --yes to delete without asking");
    let out = env.run(&["delete", "bob", "--styled"]);
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).starts_with("error: delete asks before it acts"),
        "{}",
        stderr(&out)
    );
    assert!(env.state.card("BOB-BERG").is_some());
    assert!(env.writes().is_empty());
    let out = env.json(&["delete", "bob", "--yes"]);
    assert_eq!(out["contact"], json!({ "id": "BOB-BERG", "name": "Bob Berg" }));
    assert!(env.state.card("BOB-BERG").is_none());
    assert_eq!(env.state.count(), 3);
    assert_eq!(env.error(&["show", "bob", "--cached"]), (4, "not_found".into()));
}

#[test]
fn exports_vcards() {
    let env = start();
    // As text, the vCards themselves; as JSON, inside the data.
    let text = env.styled(&["export"]);
    assert!(
        text.starts_with("BEGIN:VCARD\r\n") && text.ends_with("END:VCARD\r\n"),
        "{text}"
    );
    assert_eq!(text.matches("BEGIN:VCARD\r\n").count(), 4);
    assert!(
        text.contains("FN:Ann Lee\r\n") && text.contains("TEL;TYPE=CELL:+47 555 01 234\r\n"),
        "{text}"
    );
    let out = env.json(&["export", "--cached"]);
    assert_eq!(
        (out["action"].as_str(), out["count"].as_i64()),
        (Some("export"), Some(4))
    );
    assert_eq!(out["vcard"], text);
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
fn signed_out_is_exit_3() {
    let env = start();
    env.ok(&["sync"]);
    *env.state.signed_out.lock().unwrap() = true;
    for args in [&["list"][..], &["show", "ann"], &["export"], &["sync"]] {
        let (exit, e) = env.fails(args);
        assert_eq!((exit, e["code"].as_str()), (3, Some("sign_in_required")), "{args:?}");
        let hint = e["hint"].as_str().unwrap();
        assert!(hint.starts_with("run `icloud-session sign-in`"), "{hint}");
    }
    let out = env.run(&["list", "--styled"]);
    assert_eq!((code(&out), stdout(&out).as_str()), (3, ""));
    assert!(
        stderr(&out).starts_with("error: sign in to iCloud required"),
        "{}",
        stderr(&out)
    );
    // What the last sync left is still readable.
    assert_eq!(env.ok(&["list", "--cached", "--count"]), "4\n");
}

#[test]
fn sync_counts_changes() {
    let env = start();
    assert_eq!(env.json(&["sync"]), json!({ "contacts": 4, "changed": 4 }));
    assert_eq!(env.json(&["sync"]), json!({ "contacts": 4, "changed": 0 }));
    env.state.put(json!({ "contactId": "NEW", "firstName": "Dee" }));
    assert_eq!(env.json(&["sync"]), json!({ "contacts": 5, "changed": 1 }));
    assert_eq!(env.styled(&["sync"]), "Synced: 5 contacts (0 changed)\n");
    // `--count` is how many changed.
    env.state.put(json!({ "contactId": "NEWER", "firstName": "Eve" }));
    assert_eq!(env.ok(&["sync", "--count"]), "1\n");
}

/// `icloud-contacts` is what agents and scripts run: it must not load GTK.
#[test]
fn the_command_line_binary_does_not_link_gtk() {
    assert_cli_links_no_gtk(
        env!("CARGO_BIN_EXE_icloud-contacts"),
        option_env!("CARGO_BIN_EXE_icloud-contacts-app"),
        &["libgtk-4.", "libadwaita-1."],
    );
}

#[test]
fn no_command_runs_the_app_beside_it() {
    assert_no_command_runs_the_app(env!("CARGO_BIN_EXE_icloud-contacts"), "icloud-contacts");
}
