//! Recorded CLI scenarios (tests/differential/README.md): every scenario in
//! tests/differential/scenarios.json is run through the `icloud-notes-sync`
//! binary on its cassette (ReplayTransport via `ICLOUD_NOTES_SYNC_CASSETTE`,
//! clock and randomness pinned via `ICLOUD_NOTES_SYNC_NOW` /
//! `ICLOUD_NOTES_SYNC_DETERMINISTIC`) and compared with what an earlier run
//! recorded in tests/differential/expected/<scenario>/: exit code, `--json`
//! stdout, request log, the vault tree (state.json's `generator` normalized)
//! and the deterministic file mtimes.
//!
//! Regenerate expected/ after an intended behaviour change, then review the
//! diff before committing it:
//!
//! ```text
//! ICLOUD_NOTES_SYNC_REGEN=1 cargo test -p icloud-notes-sync --test cli_differential
//! ```
//!
//! `ICLOUD_NOTES_SYNC_DIFF_ONLY=name[,name]` limits either run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Map, Value};

const STATE_DIR: &str = ".icloud-notes";
const LEGACY_STATE_DIR: &str = ".icloud-md";

fn here() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential")
}

fn apply_edit(vault: &Path, edit: &Map<String, Value>) {
    let file = edit["file"].as_str().expect("edit.file");
    let path = vault.join(file);
    if edit.get("delete").and_then(Value::as_bool) == Some(true) {
        std::fs::remove_file(&path).unwrap();
    } else if let Some(to) = edit.get("renameTo").and_then(Value::as_str) {
        std::fs::rename(&path, vault.join(to)).unwrap();
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

/// Every file under `vault` except the state directories (`.icloud-notes`,
/// a layout 3 vault's `.icloud-md`, its backup `.icloud-md.bak-*`).
fn working_files(vault: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if dir == root && (name == STATE_DIR || name.starts_with(LEGACY_STATE_DIR)) {
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

/// `"generator": "..."` → a placeholder, so a version bump changes nothing.
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

/// `icloud-notes-<16 hex>.lock` (vault-info's `lockPath`, named after the
/// run's temp dir) → `icloud-notes-<hash>.lock`.
fn normalize_lock_hash(text: &str) -> String {
    const PREFIX: &str = "icloud-notes-";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        let is_hash = after.len() >= 21
            && after.as_bytes()[..16].iter().all(u8::is_ascii_hexdigit)
            && after[16..].starts_with(".lock");
        out.push_str(&rest[..at + PREFIX.len()]);
        if is_hash {
            out.push_str("<hash>");
            rest = &after[16..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
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

/// What one run of the binary on a scenario left behind.
struct Run {
    /// Holds `vault/`, `requests.json` and `stdout.json`; removed on drop.
    tmp: tempfile::TempDir,
    exit: i32,
    /// `--json` stdout, the run's temp dir replaced by `@OUT@`.
    stdout: String,
    stderr: String,
    started_ms: i64,
}

impl Run {
    fn out(&self) -> PathBuf {
        self.tmp.path().canonicalize().unwrap()
    }
    fn vault(&self) -> PathBuf {
        self.out().join("vault")
    }
    fn log_path(&self) -> PathBuf {
        self.out().join("requests.json")
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Prepares the scenario's vault (`vaultFrom` copy, `edits`, setup mtimes)
/// and runs the `icloud-notes-sync` binary on its cassette.
fn run_binary(scenario: &Scenario) -> Run {
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
    std::fs::create_dir_all(out.join("home")).unwrap();

    let started_ms = now_ms();
    let output = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(&args)
        .current_dir(&cwd)
        .env("HOME", out.join("home"))
        .env("XDG_RUNTIME_DIR", out.join("home")) // the vault lock, outside the compared tree
        .env("ICLOUD_NOTES_SYNC_CASSETTE", &cassette)
        .env("ICLOUD_NOTES_SYNC_REQUEST_LOG", out.join("requests.json"))
        .env("ICLOUD_NOTES_SYNC_NOW", scenario.now.to_string())
        .env("ICLOUD_NOTES_SYNC_DETERMINISTIC", "1")
        .output()
        .unwrap();
    let stdout =
        normalize_lock_hash(&String::from_utf8_lossy(&output.stdout).replace(&*out.to_string_lossy(), "@OUT@"));
    std::fs::write(out.join("stdout.json"), &stdout).unwrap();
    Run {
        tmp,
        exit: output.status.code().unwrap_or(-1),
        stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        started_ms,
    }
}

/// The vault's files with state.json's `generator` normalized.
fn vault_files(vault: &Path) -> BTreeMap<String, Vec<u8>> {
    all_files(vault)
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
}

/// The mtimes of the vault's files that are deterministic: set from note
/// dates or by the setup (well before the run started), not by the write.
fn deterministic_mtimes(run: &Run) -> BTreeMap<String, i64> {
    let vault = run.vault();
    working_files(&vault)
        .into_iter()
        .map(|p| {
            (
                p.strip_prefix(&vault).unwrap().to_string_lossy().into_owned(),
                mtime_ms(&p),
            )
        })
        .filter(|(_, ms)| *ms < run.started_ms - 5000)
        .collect()
}

/// Records `run` as `expected/<name>/` (`ICLOUD_NOTES_SYNC_REGEN=1`).
fn record(run: &Run, name: &str) {
    let dest = here().join("expected").join(name);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("exit"), format!("{}\n", run.exit)).unwrap();
    std::fs::write(dest.join("stdout.json"), &run.stdout).unwrap();
    // No log means no requests were sent; recorded as an empty one, so a
    // later run that starts sending requests is caught.
    match std::fs::read(run.log_path()) {
        Ok(log) => std::fs::write(dest.join("requests.json"), log).unwrap(),
        Err(_) => std::fs::write(dest.join("requests.json"), "{\n  \"requests\": []\n}\n").unwrap(),
    }
    let vault = run.vault();
    if vault.is_dir() {
        for (rel, bytes) in vault_files(&vault) {
            let path = dest.join("vault").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let mut mtimes = serde_json::to_string_pretty(&deterministic_mtimes(run)).unwrap();
        mtimes.push('\n');
        std::fs::write(dest.join("mtimes.json"), mtimes).unwrap();
    }
}

/// Compares `run` with `expected/<name>/`; returns the mismatches.
fn compare(scenario: &Scenario, run: &Run, name: &str) -> Vec<String> {
    let expected = here().join("expected").join(name);
    let compare: Vec<String> = match scenario.raw.get("compare").and_then(Value::as_array) {
        Some(list) => list.iter().map(|v| v.as_str().unwrap().to_owned()).collect(),
        None => ["exit", "stdout", "requests", "vault", "mtimes"]
            .map(String::from)
            .to_vec(),
    };
    let wants = |what: &str| compare.iter().any(|c| c == what);
    let mut failures = Vec::new();
    let stderr = &run.stderr;

    if wants("exit") {
        let want: i32 = std::fs::read_to_string(expected.join("exit"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        if want != run.exit {
            failures.push(format!("exit: expected {want}, got {}\nstderr:\n{stderr}", run.exit));
        }
    }
    if wants("stdout") {
        let want = std::fs::read_to_string(expected.join("stdout.json")).unwrap_or_default();
        if want != run.stdout {
            failures.push(format!(
                "stdout differs:\n--- expected\n{want}\n--- got\n{}\nstderr:\n{stderr}",
                run.stdout
            ));
        }
    }
    if wants("requests") && expected.join("requests.json").exists() {
        let read = |path: &Path| -> Vec<Value> {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v["requests"].as_array().cloned())
                .unwrap_or_default()
        };
        let (want, got) = (read(&expected.join("requests.json")), read(&run.log_path()));
        if want != got {
            failures.push(format!(
                "request log differs:\n--- expected\n{}\n--- got\n{}",
                serde_json::to_string_pretty(&want).unwrap(),
                serde_json::to_string_pretty(&got).unwrap()
            ));
        }
    }
    if wants("vault") {
        let want = vault_files(&expected.join("vault"));
        let got = vault_files(&run.vault());
        let names: std::collections::BTreeSet<&String> = want.keys().chain(got.keys()).collect();
        for name in names {
            match (want.get(name), got.get(name)) {
                (Some(_), None) => failures.push(format!("vault: {name} missing")),
                (None, Some(_)) => failures.push(format!("vault: {name} unexpected")),
                (Some(a), Some(b)) if a != b => failures.push(format!(
                    "vault: {name} differs\n--- expected\n{}\n--- got\n{}",
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
        let vault = run.vault();
        for (file, ms) in want {
            let path = vault.join(&file);
            if !path.exists() {
                continue; // reported by the vault comparison
            }
            let got = mtime_ms(&path);
            if got != ms {
                failures.push(format!("mtime of {file}: expected {ms}, got {got}"));
            }
        }
    }
    failures
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
            "{}: record it with ICLOUD_NOTES_SYNC_REGEN=1",
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

/// Runs every scenario and compares it with `expected/<name>/`, or, with
/// `ICLOUD_NOTES_SYNC_REGEN=1`, rewrites `expected/<name>/` from the run
/// (in manifest order, so a `vaultFrom` scenario starts from the vault just
/// recorded). `ICLOUD_NOTES_SYNC_DIFF_ONLY=name[,name]` limits either.
#[test]
fn differential_scenarios() {
    let only: Option<Vec<String>> = std::env::var("ICLOUD_NOTES_SYNC_DIFF_ONLY")
        .ok()
        .map(|v| v.split(',').map(str::to_owned).collect());
    let regen = std::env::var_os("ICLOUD_NOTES_SYNC_REGEN").is_some_and(|v| v != "0");
    let mut report = Vec::new();
    for scenario in scenarios() {
        if only.as_ref().is_some_and(|only| !only.contains(&scenario.name)) {
            continue;
        }
        let run = run_binary(&scenario);
        if regen {
            record(&run, &scenario.name);
            eprintln!("recorded {}: exit {}", scenario.name, run.exit);
            continue;
        }
        let failures = compare(&scenario, &run, &scenario.name);
        if !failures.is_empty() {
            report.push(format!("## {}\n{}", scenario.name, failures.join("\n\n")));
        }
    }
    assert!(report.is_empty(), "scenario mismatches:\n\n{}", report.join("\n\n"));
}

// The checks below pin down what a few recorded scenarios must show, so a
// regen that changes that behaviour fails here instead of slipping into
// expected/ unnoticed (`differential_scenarios` keeps the binary in line
// with expected/).

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn expected(name: &str) -> PathBuf {
    here().join("expected").join(name)
}

/// `.md` files under `vault` (outside the state dir) as relative paths.
fn note_files(vault: &Path) -> Vec<String> {
    working_files(vault)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|p| p.strip_prefix(vault).unwrap().to_string_lossy().into_owned())
        .collect()
}

fn requests(name: &str) -> Vec<Value> {
    read_json(&expected(name).join("requests.json"))["requests"]
        .as_array()
        .unwrap()
        .clone()
}

const LOOKUP: &str = "/database/1/com.apple.notes/production/private/records/lookup";
const FRESH: &str = "5f1d0c3a-7b2e-4c9a-9e61-2b8d4a6c0f17";

/// `dup-clone`: the private `changes/zone` listing repeats the note on its
/// second page (what live clones hit about 1 in 9 times). Both pages are
/// fetched, but the note is written once - the same vault and summary as
/// the single-listing `tiny-clone`, not a second untracked copy.
#[test]
fn dup_clone_writes_a_repeated_record_once() {
    let zone_pages = requests("dup-clone")
        .iter()
        .filter(|r| r["path"].as_str().is_some_and(|p| p.ends_with("/private/changes/zone")))
        .count();
    assert_eq!(zone_pages, 2);
    assert_eq!(note_files(&expected("dup-clone/vault")), ["Notes/Test Note.md"]);
    assert_eq!(
        vault_files(&expected("dup-clone/vault")),
        vault_files(&expected("tiny-clone/vault"))
    );
    assert_eq!(
        read_json(&expected("dup-clone/stdout.json")),
        read_json(&expected("tiny-clone/stdout.json"))
    );
}

/// The one private `records/lookup` a `bodyless-*` scenario sends: for the
/// note the listing carried without its text.
fn assert_looks_up_fresh(name: &str) {
    let lookups: Vec<Value> = requests(name).into_iter().filter(|r| r["path"] == LOOKUP).collect();
    assert_eq!(lookups.len(), 1, "{name}");
    assert_eq!(
        lookups[0]["body"]["records"],
        serde_json::json!([{ "recordName": FRESH }]),
        "{name}"
    );
}

fn assert_fresh_written(name: &str) {
    let vault = expected(name).join("vault");
    assert_eq!(
        std::fs::read_to_string(vault.join("Notes/Fresh.md")).unwrap(),
        format!("---\napple-note-id: {FRESH}\n---\n\n# Fresh\nA note made locally.")
    );
    let state = read_json(&vault.join(".icloud-notes/state.json"));
    assert_eq!(state["notes"][FRESH]["file"], "Notes/Fresh.md");
    assert_eq!(state["notes"][FRESH]["recordChangeTag"], "27a");
}

/// The single warning of an `*-unfilled` scenario.
fn warning(stdout: &Value) -> String {
    let notices = stdout["notices"].as_array().unwrap();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["level"], "warn");
    notices[0]["message"].as_str().unwrap().to_owned()
}

/// `bodyless-pull`: a pull lists a new note without its text; it is looked
/// up, added, and the sync token moves on.
#[test]
fn bodyless_pull_looks_the_new_note_up_and_adds_it() {
    assert_looks_up_fresh("bodyless-pull");
    let stdout = read_json(&expected("bodyless-pull/stdout.json"));
    assert_eq!(stdout["added"], 1);
    assert_eq!(stdout["skippedNewUnsyncable"], 0);
    assert_eq!(stdout["notices"], serde_json::json!([]));
    assert_fresh_written("bodyless-pull");
    let state = read_json(&expected("bodyless-pull/vault/.icloud-notes/state.json"));
    assert_eq!(state["syncToken"], "AQAAAAAAAAAD");
}

/// `bodyless-pull-unfilled`: the lookup comes back without the text too.
/// The note is skipped, the previous private sync token is kept (so the next
/// pull sees the note again), and a warning says so.
#[test]
fn bodyless_pull_unfilled_keeps_the_previous_sync_token() {
    assert_looks_up_fresh("bodyless-pull-unfilled");
    let stdout = read_json(&expected("bodyless-pull-unfilled/stdout.json"));
    assert_eq!(stdout["skippedNewUnsyncable"], 1);
    let message = warning(&stdout);
    assert!(
        message.contains("1 new note(s)") && message.contains("next pull"),
        "{message}"
    );
    let state = read_json(&expected("bodyless-pull-unfilled/vault/.icloud-notes/state.json"));
    let before = read_json(&expected("tiny-clone/vault/.icloud-notes/state.json"));
    assert_eq!(state["syncToken"], before["syncToken"], "the previous token is kept");
    assert!(state["notes"].get(FRESH).is_none());
}

/// `bodyless-clone`: clone lists a note without its text; it is looked up
/// and written beside Test Note, and the private sync token is saved.
#[test]
fn bodyless_clone_looks_the_new_note_up_and_writes_it() {
    assert_looks_up_fresh("bodyless-clone");
    let stdout = read_json(&expected("bodyless-clone/stdout.json"));
    assert_eq!(stdout["written"], 2);
    assert_eq!(stdout["skippedUndecodable"], 0);
    assert_eq!(stdout["notices"], serde_json::json!([]));
    assert_fresh_written("bodyless-clone");
    let state = read_json(&expected("bodyless-clone/vault/.icloud-notes/state.json"));
    assert_eq!(state["syncToken"], "AQAAAAAAAAAB");
}

/// `bodyless-clone-unfilled`: the lookup still has no text. The note is
/// skipped, no private sync token is saved (so the first pull walks from
/// scratch and sees it again), and a warning says so.
#[test]
fn bodyless_clone_unfilled_saves_no_private_sync_token() {
    assert_looks_up_fresh("bodyless-clone-unfilled");
    let stdout = read_json(&expected("bodyless-clone-unfilled/stdout.json"));
    assert_eq!(stdout["written"], 1);
    assert_eq!(stdout["skippedUndecodable"], 1);
    let message = warning(&stdout);
    assert!(
        message.contains("1 note(s)") && message.contains("first pull"),
        "{message}"
    );
    let state = read_json(&expected("bodyless-clone-unfilled/vault/.icloud-notes/state.json"));
    assert!(state.get("syncToken").is_none(), "{state}");
    assert_eq!(
        note_files(&expected("bodyless-clone-unfilled/vault")),
        ["Notes/Test Note.md"]
    );
}

/// `tiny-sync`: one run pushes the local edit, then pulls, over one
/// connection - the request log (a cassette transport opened afresh would
/// start a new one) holds both halves' requests, push's first - and each
/// half's result is what `push` and `pull` alone give.
#[test]
fn tiny_sync_pushes_then_pulls_over_one_connection() {
    let paths: Vec<String> = requests("tiny-sync")
        .iter()
        .map(|r| {
            r["path"]
                .as_str()
                .unwrap()
                .rsplit("/production/")
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        paths,
        [
            "private/records/lookup",
            "private/records/modify",
            "private/changes/zone",
            "shared/changes/database"
        ]
    );
    let sync = read_json(&expected("tiny-sync/stdout.json"));
    assert_eq!(sync["push"]["result"], read_json(&expected("tiny-push/stdout.json")));
    assert_eq!(
        sync["pull"]["result"],
        read_json(&expected("tiny-pull-noop/stdout.json"))
    );
    assert_eq!(sync["vault_info"]["notes"].as_array().unwrap().len(), 1);
    let clean = read_json(&expected("tiny-sync-clean/stdout.json"));
    assert_eq!(
        (
            clean["push"]["result"]["pushed"].as_u64(),
            clean["pull"]["ok"].as_bool()
        ),
        (Some(0), Some(true))
    );
}
/// `attach-clone` / `attach-pull`: two notes with one file attachment each.
/// Their Attachment records are looked up in one `records/lookup`, their
/// Media records in a second (not two per note), both files are downloaded
/// and linked, and state tracks them.
fn assert_attachments_batched(name: &str) {
    let lookups: Vec<Value> = requests(name).into_iter().filter(|r| r["path"] == LOOKUP).collect();
    let names = |r: &Value| -> Vec<String> {
        r["body"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["recordName"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(lookups.len(), 2, "{name}");
    assert_eq!(
        names(&lookups[0]),
        [
            "7DAFDA6F-4AC4-41D8-9958-049373B80824",
            "7ED80274-4400-4C02-87EA-F542F056FF02"
        ],
        "{name}: both Attachment records"
    );
    assert_eq!(
        names(&lookups[1]),
        [
            "0B8509A3-A5FC-470B-A777-03BFFFDFB5F9",
            "066C8A2E-796F-403F-AD75-A5267CBD0E18"
        ],
        "{name}: both Media records"
    );
    let downloads = requests(name).into_iter().filter(|r| r["method"] == "GET").count();
    assert_eq!(downloads, 2, "{name}");
    let vault = expected(name).join("vault");
    assert_eq!(
        std::fs::read(vault.join("Notes/attachments/Call with Janice Elkins.m4a")).unwrap(),
        b"audio bytes"
    );
    assert_eq!(
        std::fs::read(vault.join("Notes/attachments/_7130093.jpeg")).unwrap(),
        b"jpeg  bytes"
    );
    let state = read_json(&vault.join(".icloud-notes/state.json"));
    assert_eq!(state["attachments"].as_object().unwrap().len(), 2, "{name}");
}

#[test]
fn attach_clone_looks_attachments_up_once_per_zone() {
    assert_attachments_batched("attach-clone");
    assert_eq!(
        read_json(&expected("attach-clone/stdout.json"))["attachmentsDownloaded"],
        2
    );
}

#[test]
fn attach_pull_looks_attachments_up_once_per_zone() {
    assert_attachments_batched("attach-pull");
    let stdout = read_json(&expected("attach-pull/stdout.json"));
    assert_eq!(stdout["added"], 2);
    assert_eq!(stdout["attachmentsDownloaded"], 2);
}

const SHARED_DB: &str = "/database/1/com.apple.notes/production/shared/changes/database";
const SHARED_ZONE: &str = "/database/1/com.apple.notes/production/shared/changes/zone";
const SHARER: &str = "_0123456789abcdef0123456789abcdef";
const SHARED_NOTE: &str = "9d8c7b6a-5f4e-4d3c-8b2a-192837465abc";

/// The bodies of a scenario's shared `changes/database` requests, and how
/// many shared `changes/zone` requests it sent.
fn shared_requests(name: &str) -> (Vec<Value>, usize) {
    let all = requests(name);
    let listings = all
        .iter()
        .filter(|r| r["path"] == SHARED_DB)
        .map(|r| r["body"].clone())
        .collect();
    (listings, all.iter().filter(|r| r["path"] == SHARED_ZONE).count())
}

fn state_of(name: &str) -> Value {
    read_json(&expected(name).join("vault/.icloud-notes/state.json"))
}

/// `shared-pull-unchanged`: the stored shared-database token is resumed,
/// no zone changed, so no shared `changes/zone` is sent - and the zone, its
/// token, its sharer home and its note all stay as they were.
#[test]
fn shared_pull_unchanged_resumes_the_listing_and_walks_no_zone() {
    let (listings, zone_walks) = shared_requests("shared-pull-unchanged");
    assert_eq!(listings, [serde_json::json!({ "syncToken": "AQAAAAAAAAAS" })]);
    assert_eq!(zone_walks, 0);
    let before = state_of("asset-clone");
    let after = state_of("shared-pull-unchanged");
    assert_eq!(after["sharedZoneSyncTokens"], before["sharedZoneSyncTokens"]);
    assert_eq!(after["sharerHomes"], before["sharerHomes"]);
    assert_eq!(after["notes"], before["notes"]);
    assert_eq!(after["sharedDatabase"]["syncToken"], "AQAAAAAAAAAU");
    assert_eq!(after["sharedDatabase"]["zones"], before["sharedDatabase"]["zones"]);
    assert_eq!(
        after["sharedDatabase"]["listedAt"],
        before["sharedDatabase"]["listedAt"]
    );
    assert_eq!(
        note_files(&expected("shared-pull-unchanged/vault")),
        note_files(&expected("asset-clone/vault"))
    );
}

/// `shared-pull-revoked`: the incremental listing marks the zone deleted;
/// its note is untracked (file kept) as a full listing without it would.
#[test]
fn shared_pull_revoked_untracks_the_zones_notes() {
    let (_, zone_walks) = shared_requests("shared-pull-revoked");
    assert_eq!(zone_walks, 0);
    let stdout = read_json(&expected("shared-pull-revoked/stdout.json"));
    assert_eq!(stdout["unsharedUntracked"], 1);
    let state = state_of("shared-pull-revoked");
    assert!(state["notes"].get(SHARED_NOTE).is_none());
    assert_eq!(state["sharedDatabase"]["zones"], serde_json::json!([]));
    assert!(state["sharedZoneSyncTokens"].get(SHARER).is_none());
    assert!(
        expected("shared-pull-revoked/vault")
            .join(SHARER)
            .join("Shared Big Note.md")
            .exists()
    );
}

/// A rejected token, an old state without `sharedDatabase`, and a cursor
/// older than a day each list the shared zones from scratch and walk them.
#[test]
fn shared_pull_lists_from_scratch_when_the_cursor_cannot_be_used() {
    for (name, listings) in [
        (
            "shared-pull-token-rejected",
            vec![
                serde_json::json!({ "syncToken": "AQAAAAAAAAAS" }),
                serde_json::json!({}),
            ],
        ),
        ("shared-pull-old-state", vec![serde_json::json!({})]),
        ("shared-pull-stale-cursor", vec![serde_json::json!({})]),
    ] {
        assert_eq!(shared_requests(name), (listings, 1), "{name}");
        let state = state_of(name);
        assert_eq!(state["sharedDatabase"]["syncToken"], "AQAAAAAAAAAV", "{name}");
        assert_eq!(state["sharedZoneSyncTokens"][SHARER], "AQAAAAAAAAAW", "{name}");
        assert!(state["notes"].get(SHARED_NOTE).is_some(), "{name}");
    }
}

/// `tiny-migrate-v3`: restore (a command that takes the lock, and needs no
/// network) on a layout 3 vault moves its state to `.icloud-notes/`: base/
/// moved as it was, state.json there at layout 4, a
/// tombstone left in `.icloud-md/`, a backup of `.icloud-md/` beside it, and
/// the note files untouched.
#[test]
fn tiny_migrate_v3_moves_the_state_to_icloud_notes() {
    let clone = expected("tiny-clone/vault");
    let vault = expected("tiny-migrate-v3/vault");
    assert_eq!(state_of("tiny-migrate-v3")["layoutVersion"], 4);
    // (A clone records no history; vault_migrate.rs moves some.)
    assert!(clone.join(STATE_DIR).join("base").is_dir());
    assert_eq!(
        all_files(&vault.join(STATE_DIR).join("base")),
        all_files(&clone.join(STATE_DIR).join("base"))
    );
    assert_eq!(
        read_json(&vault.join(".icloud-md/state.json")),
        serde_json::json!({ "layoutVersion": 4, "movedTo": ".icloud-notes" })
    );
    assert_eq!(
        all_files(&vault.join(LEGACY_STATE_DIR)).len(),
        1,
        "only the tombstone is left"
    );
    let backups: Vec<PathBuf> = std::fs::read_dir(&vault)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".icloud-md.bak-"))
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(read_json(&backups[0].join("state.json"))["layoutVersion"], 3);
    assert_eq!(
        all_files(&backups[0].join("base")),
        all_files(&clone.join(STATE_DIR).join("base"))
    );
    let notes = |v: &Path| -> BTreeMap<String, Vec<u8>> {
        note_files(v)
            .into_iter()
            .map(|f| (f.clone(), std::fs::read(v.join(&f)).unwrap()))
            .collect()
    };
    assert_eq!(notes(&vault), notes(&clone));
}

/// `tiny-status-v3`: status reads a layout 3 vault where it is and moves
/// nothing; `tiny-status-v2`: an older layout is refused, not migrated, by
/// status; `tiny-migrate-v2`: push takes a layout 2 vault through 3 to 4.
#[test]
fn read_only_commands_do_not_migrate_and_locked_ones_go_all_the_way() {
    let vault = expected("tiny-status-v3/vault");
    assert!(!vault.join(STATE_DIR).exists());
    assert_eq!(read_json(&vault.join(".icloud-md/state.json"))["layoutVersion"], 3);
    assert_eq!(
        read_json(&expected("tiny-status-v3/stdout.json"))["entries"],
        serde_json::json!([])
    );

    assert!(!expected("tiny-status-v2/vault").join(STATE_DIR).exists());
    assert_eq!(std::fs::read_to_string(expected("tiny-status-v2/exit")).unwrap(), "1\n");

    assert_eq!(state_of("tiny-migrate-v2")["layoutVersion"], 4);
    assert_eq!(state_of("tiny-migrate-v2")["titleMode"], "in-body");
    assert!(
        std::fs::read_to_string(expected("tiny-migrate-v2/vault/Notes/Test Note.md"))
            .unwrap()
            .starts_with("---\napple-note-id: 03667d1d-eee8-4e98-82fb-8c5cd02fd9d1\n---\n")
    );
}
