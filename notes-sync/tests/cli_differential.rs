//! The Rust side of the differential harness (tests/differential/README.md):
//! every scenario in tests/differential/scenarios.json is run through the
//! `icloud-notes-sync` binary on its cassette (ReplayTransport via
//! `ICLOUD_NOTES_SYNC_CASSETTE`, clock and randomness pinned via
//! `ICLOUD_NOTES_SYNC_NOW` / `ICLOUD_NOTES_SYNC_DETERMINISTIC`) in a vault
//! prepared exactly as `regen.py` prepares it for icloud-md, and compared with
//! what icloud-md did (tests/differential/expected/<scenario>/): exit code,
//! `--json` stdout, request log, the vault tree (state.json's `generator`
//! normalized) and the deterministic file mtimes.
//!
//! Scenarios without an `icloudMd` ref have expectations from stock icloud-md
//! 0.6.2, so they run with `ICLOUD_NOTES_SYNC_ASSET_BODIES=0` (upstream PR
//! #29 off). The `asset-*` scenarios (`"icloudMd": "fetch-asset-note-bodies"`)
//! have expectations from the PR #29 fork branch and run with it on.
//!
//! Scenarios with a `portDeviation` key cover a deliberate difference from
//! icloud-md 0.6.2 (docs/PORT_PLAN.md §7): their expectations still come from
//! 0.6.2, but instead of a byte comparison a dedicated test asserts how the
//! two differ (`dup_clone_*`, `bodyless_pull_*`, `bodyless_clone_*`).
//!
//! `ICLOUD_NOTES_SYNC_DIFF_ONLY=name[,name]` limits the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Map, Value};

const STATE_DIR: &str = ".icloud-md";

fn here() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential")
}

fn apply_edit(vault: &Path, edit: &Map<String, Value>) {
    let file = edit["file"].as_str().expect("edit.file");
    let path = vault.join(file);
    if edit.get("delete").and_then(Value::as_bool) == Some(true) {
        std::fs::remove_file(&path).unwrap();
    } else if let Some(text) = edit.get("write").and_then(Value::as_str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    } else if let Some(text) = edit.get("append").and_then(Value::as_str) {
        let mut current = std::fs::read_to_string(&path).unwrap();
        current.push_str(text);
        std::fs::write(&path, current).unwrap();
    } else if let Some(Value::Array(pair)) = edit.get("replace") {
        let (old, new) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains(old), "{file}: {old:?} not found");
        std::fs::write(&path, current.replacen(old, new, 1)).unwrap();
    } else if let Some(Value::Object(changes)) = edit.get("json") {
        let mut data: Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (key, value) in changes {
            if value.is_null() {
                data.shift_remove(key);
            } else {
                data.insert(key.clone(), value.clone());
            }
        }
        let mut text = serde_json::to_string_pretty(&Value::Object(data)).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    } else {
        panic!("unknown edit {edit:?}");
    }
}

/// Every file under `vault` except the state directory.
fn working_files(vault: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if dir == root && entry.file_name() == STATE_DIR {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(vault, vault, &mut out);
    out.sort();
    out
}

fn all_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn set_mtime(path: &Path, ms: i64) {
    let t = UNIX_EPOCH + Duration::from_millis(ms as u64);
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
        .unwrap();
}

fn mtime_ms(path: &Path) -> i64 {
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    modified.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// `"generator": "..."` → a placeholder, so icloud-md's and ours compare.
fn normalize_generator(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("\"generator\": ") {
            let indent = &line[..line.len() - line.trim_start().len()];
            let comma = if line.trim_end().ends_with(',') { "," } else { "" };
            out.push_str(&format!("{indent}\"generator\": \"<generator>\"{comma}\n"));
        } else {
            out.push_str(line);
        }
    }
    out.into_bytes()
}

struct Scenario {
    name: String,
    raw: Map<String, Value>,
    now: i64,
    setup_mtime: i64,
}

fn subst(text: &str, out: &Path, vault: &Path) -> String {
    text.replace("@VAULT@", &vault.to_string_lossy())
        .replace("@OUT@", &out.to_string_lossy())
}

/// Runs one scenario; returns the list of mismatches.
fn run_scenario(scenario: &Scenario) -> Vec<String> {
    run_scenario_against(scenario, &scenario.name).0
}

/// Runs `scenario` and compares it with `expected/<expected_name>/`; returns
/// the mismatches and the run's temp dir (the vault is `<dir>/vault`).
fn run_scenario_against(scenario: &Scenario, expected_name: &str) -> (Vec<String>, tempfile::TempDir) {
    let expected = here().join("expected").join(expected_name);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    if let Some(from) = scenario.raw.get("vaultFrom").and_then(Value::as_str) {
        copy_dir(&here().join("expected").join(from).join("vault"), &vault);
    }
    for edit in scenario
        .raw
        .get("edits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        apply_edit(&vault, edit.as_object().unwrap());
    }
    if vault.is_dir() {
        for file in working_files(&vault) {
            set_mtime(&file, scenario.setup_mtime);
        }
    }
    let cwd = subst(
        scenario.raw.get("cwd").and_then(Value::as_str).unwrap_or("@OUT@"),
        &out,
        &vault,
    );
    let args: Vec<String> = scenario.raw["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| subst(a.as_str().unwrap(), &out, &vault))
        .collect();
    let cassette = here()
        .join("cassettes")
        .join(scenario.raw["cassette"].as_str().unwrap());
    let log_path = out.join("requests.json");
    std::fs::create_dir_all(out.join("home")).unwrap();

    let asset_bodies = if scenario.raw.contains_key("icloudMd") {
        "1"
    } else {
        "0"
    };
    let output = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(&args)
        .env("ICLOUD_NOTES_SYNC_ASSET_BODIES", asset_bodies)
        .current_dir(&cwd)
        .env("HOME", out.join("home"))
        .env("ICLOUD_NOTES_SYNC_CASSETTE", &cassette)
        .env("ICLOUD_NOTES_SYNC_REQUEST_LOG", &log_path)
        .env("ICLOUD_NOTES_SYNC_NOW", scenario.now.to_string())
        .env("ICLOUD_NOTES_SYNC_DETERMINISTIC", "1")
        .output()
        .unwrap();

    let compare: Vec<String> = match scenario.raw.get("compare").and_then(Value::as_array) {
        Some(list) => list.iter().map(|v| v.as_str().unwrap().to_owned()).collect(),
        None => ["exit", "stdout", "requests", "vault", "mtimes"]
            .map(String::from)
            .to_vec(),
    };
    let wants = |what: &str| compare.iter().any(|c| c == what);
    let mut failures = Vec::new();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Kept beside the vault for the port-deviation tests.
    std::fs::write(out.join("stdout.json"), &output.stdout).unwrap();

    if wants("exit") {
        let want: i32 = std::fs::read_to_string(expected.join("exit"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let got = output.status.code().unwrap_or(-1);
        if want != got {
            failures.push(format!("exit: icloud-md {want}, ours {got}\nstderr:\n{stderr}"));
        }
    }
    if wants("stdout") {
        let want = std::fs::read_to_string(expected.join("stdout.json")).unwrap_or_default();
        let got = String::from_utf8_lossy(&output.stdout).replace(&*out.to_string_lossy(), "@OUT@");
        if want != got {
            failures.push(format!(
                "stdout differs:\n--- icloud-md\n{want}\n--- ours\n{got}\nstderr:\n{stderr}"
            ));
        }
    }
    if wants("requests") && expected.join("requests.json").exists() {
        let node: Value =
            serde_json::from_str(&std::fs::read_to_string(expected.join("requests.json")).unwrap()).unwrap();
        let want: Vec<Value> = node["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["service"] != "setup")
            .cloned()
            .collect();
        let got: Vec<Value> = std::fs::read_to_string(&log_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["requests"].as_array().cloned())
            .unwrap_or_default();
        if want != got {
            failures.push(format!(
                "request log differs:\n--- icloud-md\n{}\n--- ours\n{}",
                serde_json::to_string_pretty(&want).unwrap(),
                serde_json::to_string_pretty(&got).unwrap()
            ));
        }
    }
    if wants("vault") {
        let want = all_files(&expected.join("vault"));
        let got = all_files(&vault);
        let norm = |files: BTreeMap<String, Vec<u8>>| -> BTreeMap<String, Vec<u8>> {
            files
                .into_iter()
                .map(|(k, v)| {
                    let v = if k == format!("{STATE_DIR}/state.json") {
                        normalize_generator(&v)
                    } else {
                        v
                    };
                    (k, v)
                })
                .collect()
        };
        let (want, got) = (norm(want), norm(got));
        let names: std::collections::BTreeSet<&String> = want.keys().chain(got.keys()).collect();
        for name in names {
            match (want.get(name), got.get(name)) {
                (Some(_), None) => failures.push(format!("vault: {name} missing from ours")),
                (None, Some(_)) => failures.push(format!("vault: {name} only in ours")),
                (Some(a), Some(b)) if a != b => failures.push(format!(
                    "vault: {name} differs\n--- icloud-md\n{}\n--- ours\n{}",
                    String::from_utf8_lossy(a),
                    String::from_utf8_lossy(b)
                )),
                _ => {}
            }
        }
    }
    if wants("mtimes") && expected.join("mtimes.json").exists() {
        let want: BTreeMap<String, i64> =
            serde_json::from_str(&std::fs::read_to_string(expected.join("mtimes.json")).unwrap()).unwrap();
        for (file, ms) in want {
            let path = vault.join(&file);
            if !path.exists() {
                continue; // reported by the vault comparison
            }
            let got = mtime_ms(&path);
            if got != ms {
                failures.push(format!("mtime of {file}: icloud-md {ms}, ours {got}"));
            }
        }
    }
    (failures, tmp)
}

fn scenarios() -> Vec<Scenario> {
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(here().join("scenarios.json")).unwrap()).unwrap();
    let defaults = &manifest["defaults"];
    manifest["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| Scenario {
            name: s["name"].as_str().unwrap().to_owned(),
            now: s.get("now").or(defaults.get("now")).and_then(Value::as_i64).unwrap(),
            setup_mtime: s
                .get("setupMtimeMs")
                .or(defaults.get("setupMtimeMs"))
                .and_then(Value::as_i64)
                .unwrap(),
            raw: s.as_object().unwrap().clone(),
        })
        .collect()
}

#[test]
fn scenarios_and_expectations_line_up() {
    let all = scenarios();
    assert!(!all.is_empty());
    for scenario in &all {
        let expected = here().join("expected").join(&scenario.name);
        assert!(
            expected.join("exit").exists(),
            "{}: run tests/differential/regen.py",
            scenario.name
        );
        let cassette = here()
            .join("cassettes")
            .join(scenario.raw["cassette"].as_str().unwrap());
        assert!(cassette.exists(), "{}: missing cassette", scenario.name);
        if let Some(from) = scenario.raw.get("vaultFrom").and_then(Value::as_str) {
            let position = all
                .iter()
                .position(|s| s.name == from)
                .expect("vaultFrom names a scenario");
            let own = all.iter().position(|s| s.name == scenario.name).unwrap();
            assert!(position < own, "{}: vaultFrom must come earlier", scenario.name);
        }
    }
}

#[test]
fn generator_normalization() {
    let a = normalize_generator(b"{\n  \"layoutVersion\": 3,\n  \"generator\": \"icloud-md 0.6.2\"\n}\n");
    let b = normalize_generator(b"{\n  \"layoutVersion\": 3,\n  \"generator\": \"icloud-notes-sync 0.1.0\"\n}\n");
    assert_eq!(a, b);
}

#[test]
fn differential_scenarios() {
    let only: Option<Vec<String>> = std::env::var("ICLOUD_NOTES_SYNC_DIFF_ONLY")
        .ok()
        .map(|v| v.split(',').map(str::to_owned).collect());
    let mut report = Vec::new();
    for scenario in scenarios() {
        if only.as_ref().is_some_and(|only| !only.contains(&scenario.name)) {
            continue;
        }
        if scenario.raw.contains_key("portDeviation") {
            continue; // asserted by its own test below, not byte-compared
        }
        let failures = run_scenario(&scenario);
        if !failures.is_empty() {
            report.push(format!("## {}\n{}", scenario.name, failures.join("\n\n")));
        }
    }
    assert!(report.is_empty(), "differential mismatches:\n\n{}", report.join("\n\n"));
}

/// `.md` files under `vault` (outside the state dir) as relative paths.
fn note_files(vault: &Path) -> Vec<String> {
    working_files(vault)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|p| p.strip_prefix(vault).unwrap().to_string_lossy().into_owned())
        .collect()
}

fn tracked_files(vault: &Path) -> Vec<String> {
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(vault.join(STATE_DIR).join("state.json")).unwrap()).unwrap();
    state["notes"]
        .as_object()
        .unwrap()
        .values()
        .map(|n| n["file"].as_str().unwrap().to_owned())
        .collect()
}

/// `dup-clone` (portDeviation `dedupe-records`): the private `changes/zone`
/// listing repeats the tiny-clone note on its second page. icloud-md 0.6.2
/// writes it twice - a byte-identical, untracked "Test Note.md" beside the
/// tracked "Test Note 2.md", which its next push would create as a second
/// note. The port writes it once: same requests as icloud-md, and a vault,
/// stdout and mtimes identical to icloud-md's single-listing `tiny-clone`.
#[test]
fn dup_clone_icloud_md_duplicates_and_the_port_does_not() {
    let all = scenarios();
    let scenario = all.iter().find(|s| s.name == "dup-clone").expect("dup-clone scenario");
    assert_eq!(scenario.raw["portDeviation"], "dedupe-records");

    // What 0.6.2 did (recorded by regen.py): two copies, one untracked.
    let node_vault = here().join("expected/dup-clone/vault");
    let node_files = note_files(&node_vault);
    assert_eq!(node_files, ["Notes/Test Note 2.md", "Notes/Test Note.md"]);
    assert_eq!(
        std::fs::read(node_vault.join(&node_files[0])).unwrap(),
        std::fs::read(node_vault.join(&node_files[1])).unwrap(),
        "icloud-md's two copies are byte-identical"
    );
    assert_eq!(tracked_files(&node_vault), ["Notes/Test Note 2.md"]);
    let node_stdout: Value =
        serde_json::from_str(&std::fs::read_to_string(here().join("expected/dup-clone/stdout.json")).unwrap()).unwrap();
    assert_eq!(node_stdout["written"], 2);

    // The port sends the same requests (both pages are still fetched)...
    let requests_only = Scenario {
        name: scenario.name.clone(),
        raw: {
            let mut raw = scenario.raw.clone();
            raw.insert("compare".into(), serde_json::json!(["exit", "requests"]));
            raw
        },
        now: scenario.now,
        setup_mtime: scenario.setup_mtime,
    };
    let (failures, _) = run_scenario_against(&requests_only, "dup-clone");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));

    // ...but writes the note once: exactly icloud-md's tiny-clone result.
    let outputs = Scenario {
        name: scenario.name.clone(),
        raw: {
            let mut raw = scenario.raw.clone();
            raw.insert(
                "compare".into(),
                serde_json::json!(["exit", "stdout", "vault", "mtimes"]),
            );
            raw
        },
        now: scenario.now,
        setup_mtime: scenario.setup_mtime,
    };
    let (failures, tmp) = run_scenario_against(&outputs, "tiny-clone");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    let port_vault = tmp.path().canonicalize().unwrap().join("vault");
    assert_eq!(note_files(&port_vault), ["Notes/Test Note.md"]);
    assert_eq!(tracked_files(&port_vault), ["Notes/Test Note.md"]);
}

/// A pull scenario's `requests.json` entries, `setup` dropped.
fn node_requests(name: &str) -> Vec<Value> {
    let log: Value = serde_json::from_str(
        &std::fs::read_to_string(here().join("expected").join(name).join("requests.json")).unwrap(),
    )
    .unwrap();
    log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["service"] != "setup")
        .cloned()
        .collect()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const LOOKUP: &str = "/database/1/com.apple.notes/production/private/records/lookup";
const FRESH: &str = "5f1d0c3a-7b2e-4c9a-9e61-2b8d4a6c0f17";

/// Runs a `bodyless-pull*` or `bodyless-clone*` scenario (portDeviation
/// `look-up-new-bodyless-notes`) with nothing compared, after checking what
/// icloud-md 0.6.2 did: skipped the new note listed without its text, never
/// looked it up, and saved the new sync token past it - so it would never be
/// added until it changed again. Returns the run's temp dir.
fn run_bodyless(name: &str) -> tempfile::TempDir {
    let all = scenarios();
    let scenario = all.iter().find(|s| s.name == name).expect("scenario");
    assert_eq!(scenario.raw["portDeviation"], "look-up-new-bodyless-notes");
    let expected = here().join("expected").join(name);
    let node_stdout = read_json(&expected.join("stdout.json"));
    let token = if name.starts_with("bodyless-clone") {
        assert_eq!(node_stdout["skippedUndecodable"], 1);
        assert_eq!(node_stdout["written"], 1, "only Test Note");
        "AQAAAAAAAAAB"
    } else {
        assert_eq!(node_stdout["skippedNewUnsyncable"], 1);
        assert_eq!(node_stdout["added"], 0);
        "AQAAAAAAAAAD"
    };
    assert!(node_requests(name).iter().all(|r| r["path"] != LOOKUP));
    let node_state = read_json(&expected.join("vault/.icloud-md/state.json"));
    assert_eq!(node_state["syncToken"], token, "icloud-md moved past the note");
    assert!(node_state["notes"].get(FRESH).is_none());

    let mut raw = scenario.raw.clone();
    raw.insert("compare".into(), serde_json::json!(["exit"]));
    let run = Scenario {
        name: scenario.name.clone(),
        raw,
        now: scenario.now,
        setup_mtime: scenario.setup_mtime,
    };
    let (failures, tmp) = run_scenario_against(&run, name);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));

    // The port sends icloud-md's requests plus one private lookup of the note.
    let out = tmp.path().canonicalize().unwrap();
    let ours: Vec<Value> = read_json(&out.join("requests.json"))["requests"]
        .as_array()
        .unwrap()
        .clone();
    let (lookups, rest): (Vec<Value>, Vec<Value>) = ours.into_iter().partition(|r| r["path"] == LOOKUP);
    assert_eq!(rest, node_requests(name));
    assert_eq!(lookups.len(), 1);
    assert_eq!(
        lookups[0]["body"]["records"],
        serde_json::json!([{ "recordName": FRESH }])
    );
    tmp
}

/// The lookup returns the note's text: the port adds the note and moves
/// the sync token on, as if the listing had carried the text.
#[test]
fn bodyless_pull_looks_the_new_note_up_and_adds_it() {
    let tmp = run_bodyless("bodyless-pull");
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    let stdout = read_json(&out.join("stdout.json"));
    assert_eq!(stdout["added"], 1);
    assert_eq!(stdout["skippedNewUnsyncable"], 0);
    assert_eq!(stdout["notices"], serde_json::json!([]));
    assert_eq!(
        std::fs::read_to_string(vault.join("Notes/Fresh.md")).unwrap(),
        format!("---\napple-note-id: {FRESH}\n---\n\n# Fresh\nA note made locally.")
    );
    let state = read_json(&vault.join(".icloud-md/state.json"));
    assert_eq!(state["syncToken"], "AQAAAAAAAAAD");
    assert_eq!(state["notes"][FRESH]["file"], "Notes/Fresh.md");
    assert_eq!(state["notes"][FRESH]["recordChangeTag"], "27a");
    // The note that was already there is untouched, as in icloud-md's run.
    let node_vault = here().join("expected/bodyless-pull/vault");
    assert_eq!(
        std::fs::read(vault.join("Notes/Test Note.md")).unwrap(),
        std::fs::read(node_vault.join("Notes/Test Note.md")).unwrap()
    );
}

/// The lookup comes back without the text too: the note is still skipped,
/// but the private sync token stays where it was (so the next pull sees
/// the note again) and a warning says so. Otherwise the vault is icloud-md's.
#[test]
fn bodyless_pull_unfilled_keeps_the_previous_sync_token() {
    let tmp = run_bodyless("bodyless-pull-unfilled");
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    let mut stdout = read_json(&out.join("stdout.json"));
    let notices = stdout["notices"].as_array().unwrap().clone();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["level"], "warn");
    let message = notices[0]["message"].as_str().unwrap();
    assert!(
        message.contains("1 new note(s)") && message.contains("next pull"),
        "{message}"
    );
    stdout["notices"] = serde_json::json!([]);
    assert_eq!(
        stdout,
        read_json(&here().join("expected/bodyless-pull-unfilled/stdout.json")),
        "otherwise icloud-md's summary: skippedNewUnsyncable 1"
    );

    let state = read_json(&vault.join(".icloud-md/state.json"));
    let before = read_json(&here().join("expected/tiny-clone/vault/.icloud-md/state.json"));
    assert_eq!(state["syncToken"], before["syncToken"], "the previous token is kept");
    assert_ne!(state["syncToken"], "AQAAAAAAAAAD");

    // Everything else matches icloud-md's run: same files, and the same
    // state.json bar the token (and the generator).
    let node_vault = here().join("expected/bodyless-pull-unfilled/vault");
    let (want, got) = (all_files(&node_vault), all_files(&vault));
    assert_eq!(want.keys().collect::<Vec<_>>(), got.keys().collect::<Vec<_>>());
    for (name, bytes) in &want {
        if name == &format!("{STATE_DIR}/state.json") {
            let mut node_state: Value = serde_json::from_slice(&normalize_generator(bytes)).unwrap();
            node_state["syncToken"] = before["syncToken"].clone();
            let ours: Value = serde_json::from_slice(&normalize_generator(&got[name])).unwrap();
            assert_eq!(ours, node_state);
        } else {
            assert_eq!(&got[name], bytes, "{name}");
        }
    }
}

/// Clone, the lookup returning the note's text: the port writes it beside
/// Test Note and saves the private sync token, as if the listing had
/// carried the text.
#[test]
fn bodyless_clone_looks_the_new_note_up_and_writes_it() {
    let tmp = run_bodyless("bodyless-clone");
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    let stdout = read_json(&out.join("stdout.json"));
    assert_eq!(stdout["written"], 2);
    assert_eq!(stdout["skippedUndecodable"], 0);
    assert_eq!(stdout["notices"], serde_json::json!([]));
    assert_eq!(
        std::fs::read_to_string(vault.join("Notes/Fresh.md")).unwrap(),
        format!("---\napple-note-id: {FRESH}\n---\n\n# Fresh\nA note made locally.")
    );
    let state = read_json(&vault.join(".icloud-md/state.json"));
    assert_eq!(state["syncToken"], "AQAAAAAAAAAB");
    assert_eq!(state["notes"][FRESH]["file"], "Notes/Fresh.md");
    assert_eq!(state["notes"][FRESH]["recordChangeTag"], "27a");
    let node_vault = here().join("expected/bodyless-clone/vault");
    assert_eq!(
        std::fs::read(vault.join("Notes/Test Note.md")).unwrap(),
        std::fs::read(node_vault.join("Notes/Test Note.md")).unwrap()
    );
}

/// Clone, the lookup still without the text: the note is skipped as in
/// icloud-md, but no private sync token is saved (so the first pull walks
/// from scratch and sees the note again) and a warning says so. Otherwise
/// the vault is icloud-md's.
#[test]
fn bodyless_clone_unfilled_saves_no_private_sync_token() {
    let tmp = run_bodyless("bodyless-clone-unfilled");
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    let mut stdout = read_json(&out.join("stdout.json"));
    let notices = stdout["notices"].as_array().unwrap().clone();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["level"], "warn");
    let message = notices[0]["message"].as_str().unwrap();
    assert!(
        message.contains("1 note(s)") && message.contains("first pull"),
        "{message}"
    );
    stdout["notices"] = serde_json::json!([]);
    assert_eq!(
        stdout,
        read_json(&here().join("expected/bodyless-clone-unfilled/stdout.json")),
        "otherwise icloud-md's summary: skippedUndecodable 1"
    );

    let node_vault = here().join("expected/bodyless-clone-unfilled/vault");
    let (want, got) = (all_files(&node_vault), all_files(&vault));
    assert_eq!(want.keys().collect::<Vec<_>>(), got.keys().collect::<Vec<_>>());
    for (name, bytes) in &want {
        if name == &format!("{STATE_DIR}/state.json") {
            let mut node_state: Value = serde_json::from_slice(&normalize_generator(bytes)).unwrap();
            node_state.as_object_mut().unwrap().remove("syncToken");
            let ours: Value = serde_json::from_slice(&normalize_generator(&got[name])).unwrap();
            assert!(ours.get("syncToken").is_none(), "{ours}");
            assert_eq!(ours, node_state);
        } else {
            assert_eq!(&got[name], bytes, "{name}");
        }
    }
}
