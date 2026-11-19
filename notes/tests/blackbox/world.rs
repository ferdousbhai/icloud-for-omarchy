//! The scratch world one test runs the binary in: a temporary directory
//! holding the home, the XDG directories, the vault and the trash, a private
//! D-Bus session bus with the suite's fake icloud-session on it, and the
//! repository's stub engine. Nothing in it is the owner's: the binary gets a
//! cleared environment, so a variable this file forgets fails safe.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use crate::session::FakeSession;

/// The account the fake icloud-session is signed in to.
pub const APPLE_ID: &str = "someone@example.com";
pub const DSID: &str = "1006081438";
/// The account the engine's recorded iCloud answers (its cassettes) belong to.
pub const CASSETTE_DSID: &str = "10000000001";

pub const SIGN_IN_HINT: &str = "run `icloud-session sign-in` (a person signs in in the window)";
pub const USAGE_HINT: &str = "run `icloud-notes --help`";

/// How long one run of the binary may take: far longer than any command, so
/// only a run that would never end (a window opened by mistake) reaches it.
const RUN_LIMIT: Duration = Duration::from_secs(60);

fn repo_file(relative: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    assert!(path.exists(), "{} is missing", path.display());
    path
}

/// The repository's stub engine: canned answers, switched by
/// `ICLOUD_NOTES_SYNC_STUB_*` (its header lists them).
pub fn stub_engine() -> PathBuf {
    repo_file("tests/stubs/icloud-notes-sync")
}

/// The real engine, which the stub forwards `vault-info` to and the cassette
/// tests run: `ICLOUD_NOTES_SYNC_REAL` (notes/bin/test builds and names it),
/// else the one cargo built beside this test.
pub fn real_engine() -> PathBuf {
    let path = std::env::var_os("ICLOUD_NOTES_SYNC_REAL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let exe = std::env::current_exe().unwrap();
            exe.parent().unwrap().parent().unwrap().join("icloud-notes-sync")
        });
    assert!(
        path.is_file(),
        "the sync engine is not built at {} (cargo build -p icloud-notes-sync, or set ICLOUD_NOTES_SYNC_REAL)",
        path.display()
    );
    path
}

/// One of the engine's recorded iCloud conversations.
pub fn cassette(name: &str) -> PathBuf {
    repo_file("../notes-sync/tests/differential/cassettes").join(name)
}

/// The 64-bit FNV-1a hash the lock file's name carries.
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The tracked notes of a vault's state file: `(id, file)` or, with a third
/// entry, a note the engine holds read-only for that reason.
pub type Tracked<'a> = &'a [&'a [&'a str]];

pub struct World {
    dir: tempfile::TempDir,
    bin: PathBuf,
    bus: Child,
    address: String,
    pub session: FakeSession,
    pub vault: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        // The bus this world started, by the handle it was started with.
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

impl World {
    /// A world with an empty vault path (nothing there yet) and the fake
    /// icloud-session signed in. `None` when `ICLOUD_NOTES_BIN` names no
    /// binary: there is nothing to drive, so the test has nothing to say.
    pub fn new() -> Option<World> {
        let Some(bin) = std::env::var_os("ICLOUD_NOTES_BIN").filter(|b| !b.is_empty()) else {
            eprintln!("skipped: ICLOUD_NOTES_BIN names no icloud-notes binary (notes/bin/test sets it)");
            return None;
        };
        let bin = PathBuf::from(bin);
        assert!(bin.is_file(), "ICLOUD_NOTES_BIN={} is not a file", bin.display());
        let dir = tempfile::Builder::new()
            .prefix("icloud-notes-blackbox-")
            .tempdir()
            .unwrap();
        for sub in ["home", "cache", "config", "state", "data", "run"] {
            fs::create_dir(dir.path().join(sub)).unwrap();
        }
        fs::set_permissions(dir.path().join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        // A bus of this test's own, with no service directories: nothing
        // installed on the machine, icloud-session included, can be started on it.
        let mut bus = Command::new("dbus-daemon")
            .arg(format!(
                "--config-file={}",
                repo_file("tests/session-bus.conf").display()
            ))
            .args(["--nofork", "--print-address=1"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir.path().join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon runs");
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_string();
        assert!(address.starts_with("unix:"), "dbus-daemon printed {address:?}");
        let session = FakeSession::start(&address);
        let vault = dir.path().join("vault");
        Some(World {
            dir,
            bin,
            bus,
            address,
            session,
            vault,
        })
    }

    /// A world whose vault is the one most tests read and change: an in-body
    /// vault with `Notes` as its default folder, a note with a list, one with
    /// two conflict blocks, a read-only one, one with an attachment, one with
    /// unreadable conflict markers and a synced copy to go back to, and two
    /// untracked notes sharing a title. Modification times are a minute
    /// apart, in the order written, so "newest first" has one answer.
    pub fn seeded() -> Option<World> {
        let w = World::new()?;
        w.state(
            "in-body",
            &[
                &["id-a", "Notes/Alpha.md"],
                &["id-b", "Work/Beta.md"],
                &["id-g", "Work/Grid.md", "is too large"],
                &["id-p", "Work/Pics.md"],
                &["id-u", "Work/Tangle.md"],
            ],
            Some("Notes"),
        );
        let notes = [
            (
                "Notes/Alpha.md",
                "---\napple-note-id: id-a\n---\n# Alpha\n- [ ] milk\n- eggs\nplain line\n",
            ),
            (
                "Work/Beta.md",
                "---\napple-note-id: id-b\n---\n# Beta\nshared\n<<<<<<< local\nmine 1\n=======\ntheirs 1\n>>>>>>> remote\n\
                 middle\n<<<<<<< local\nmine 2\n=======\ntheirs 2\n>>>>>>> remote\n",
            ),
            ("Work/Grid.md", "---\napple-note-id: id-g\n---\n# Grid\nhuge\n"),
            (
                "Work/Pics.md",
                "---\napple-note-id: id-p\n---\n# Pics\n![pic.png](attachments/pic.png)\n",
            ),
            (
                "Work/Tangle.md",
                "---\napple-note-id: id-u\n---\n# Tangle\n<<<<<<< local\na\n<<<<<<< local\nb\n=======\nc\n\
                 >>>>>>> remote\n=======\nd\n>>>>>>> remote\n",
            ),
            ("Twin.md", "# Same title\n"),
            ("Notes/Twin.md", "# Same title\n"),
        ];
        for (age, (path, text)) in notes.iter().rev().enumerate() {
            w.write(path, text);
            w.set_age(path, 60 * (age as u64 + 1));
        }
        w.write("Work/attachments/pic.png", "png");
        w.write(".icloud-notes/base/id-u.md", "# Tangle\nsynced text\n");
        Some(w)
    }

    /// The seeded vault's notes, newest first.
    pub const SEEDED: [&'static str; 7] = [
        "Notes/Twin.md",
        "Twin.md",
        "Work/Tangle.md",
        "Work/Pics.md",
        "Work/Grid.md",
        "Work/Beta.md",
        "Notes/Alpha.md",
    ];

    pub fn scratch(&self) -> &Path {
        self.dir.path()
    }

    /// The trash a deleted note or folder goes to.
    pub fn trash(&self) -> PathBuf {
        self.scratch().join("data/Trash")
    }

    /// Writes the engine's state file (layout 4), which `vault-info` reads.
    pub fn state(&self, title_mode: &str, notes: Tracked, default_folder: Option<&str>) {
        let entries: Vec<String> = notes
            .iter()
            .map(|n| {
                let reason = n
                    .get(2)
                    .map(|r| format!(r#","unpublishableReason":"{r}""#))
                    .unwrap_or_default();
                format!(
                    r#""{}":{{"file":"{}","recordChangeTag":"t","modificationDate":0{reason}}}"#,
                    n[0], n[1]
                )
            })
            .collect();
        let folders = default_folder
            .map(|d| format!(r#""folders":{{"DefaultFolder-CloudKit":{{"name":"{d}","dirName":"{d}"}}}},"#))
            .unwrap_or_default();
        self.write(
            ".icloud-notes/state.json",
            format!(
                r#"{{"layoutVersion":4,"titleMode":"{title_mode}",{folders}"notes":{{{}}}}}"#,
                entries.join(",")
            ),
        );
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.vault.join(relative)
    }

    /// Writes a file of the vault, making its folders.
    pub fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// A file of the vault as text ("" when it is not there).
    pub fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).unwrap_or_default()
    }

    pub fn exists(&self, relative: &str) -> bool {
        self.path(relative).exists()
    }

    /// Sets a file's modification time to `seconds` ago.
    pub fn set_age(&self, relative: &str, seconds: u64) {
        let file = File::options().write(true).open(self.path(relative)).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    }

    /// The vault's lock file, computed here from the documented rule (not
    /// asked of the binary): `$XDG_RUNTIME_DIR/icloud-notes-<fnv>.lock`.
    pub fn lock_path(&self) -> PathBuf {
        self.lock_path_of(&self.vault)
    }

    /// The lock file of the vault at `vault`.
    pub fn lock_path_of(&self, vault: &Path) -> PathBuf {
        let key = fs::canonicalize(vault).unwrap_or_else(|_| vault.to_path_buf());
        let hash = fnv1a64(key.as_os_str().as_encoded_bytes());
        self.scratch().join(format!("run/icloud-notes-{hash:016x}.lock"))
    }

    /// Takes the vault's lock as `holder` describes itself, until the
    /// returned file is dropped.
    pub fn hold_lock(&self, holder: &str) -> File {
        self.hold_lock_of(&self.vault, holder)
    }

    /// Takes the lock of the vault at `vault`.
    pub fn hold_lock_of(&self, vault: &Path, holder: &str) -> File {
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path_of(vault))
            .unwrap();
        file.try_lock().expect("the vault's lock is free");
        file.set_len(0).unwrap();
        file.write_all(holder.as_bytes()).unwrap();
        file
    }

    /// The verbs the stub engine logged (`sync`, `pull`, `push`, with ` --yes`).
    pub fn stub_log(&self) -> Vec<String> {
        let text = fs::read_to_string(self.scratch().join("stub.log")).unwrap_or_default();
        text.lines().map(str::to_string).collect()
    }

    /// An engine that records how it was run before becoming the stub: one
    /// line per run in the returned log, `cwd=<dir> lock=<yes|no> argv=<args
    /// joined by a space>`. Pass its path as `ICLOUD_NOTES_SYNC_BIN`.
    pub fn recording_engine(&self) -> (PathBuf, PathBuf) {
        let engine = self.scratch().join("recording-engine");
        let log = self.scratch().join("engine.log");
        fs::write(
            &engine,
            format!(
                "#!/bin/bash\necho \"cwd=$PWD lock=${{ICLOUD_NOTES_LOCK_FD:+yes}}${{ICLOUD_NOTES_LOCK_FD:-no}} argv=$*\" \
                 | sed -E 's/lock=yes[0-9]+/lock=yes/' >> '{}'\nexec '{}' \"$@\"\n",
                log.display(),
                stub_engine().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
        (engine, log)
    }

    /// `icloud-notes <args>` ready to run: stdin not a terminal, stdout and
    /// stderr captured (so never a terminal either).
    pub fn cmd<I, S>(&self, args: I) -> Cmd<'_>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Cmd {
            world: self,
            args: args.into_iter().map(|a| a.as_ref().to_os_string()).collect(),
            env: Vec::new(),
            default_vault: false,
            stdin: None,
        }
    }

    /// Runs `icloud-notes <args>`.
    pub fn run<I, S>(&self, args: I) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd(args).run()
    }

    /// Runs `icloud-notes --json <args>`.
    pub fn json<I, S>(&self, args: I) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd(args).json().run()
    }

    /// Runs `icloud-notes --styled <args>`: text mode, which a pipe would
    /// otherwise not get.
    pub fn text<I, S>(&self, args: I) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd(args).styled().run()
    }

    /// The whole environment the binary runs in.
    fn environment(&self) -> Vec<(&'static str, OsString)> {
        let s = self.scratch();
        let dir = |name: &str| s.join(name).into_os_string();
        vec![
            // /usr/bin only: nothing of the owner's (a Recall on their PATH) is findable.
            ("PATH", "/usr/bin:/bin".into()),
            ("LANG", "C.UTF-8".into()),
            ("HOME", dir("home")),
            ("XDG_CACHE_HOME", dir("cache")),
            ("XDG_CONFIG_HOME", dir("config")),
            ("XDG_STATE_HOME", dir("state")),
            // The trash and the left-iCloud record; on the vault's filesystem.
            ("XDG_DATA_HOME", dir("data")),
            // Where the vault's lock file goes.
            ("XDG_RUNTIME_DIR", dir("run")),
            ("DBUS_SESSION_BUS_ADDRESS", self.address.clone().into()),
            // Should a window ever start, it draws nowhere.
            ("QT_QPA_PLATFORM", "offscreen".into()),
            ("NO_AT_BRIDGE", "1".into()),
            ("ICLOUD_NOTES_VAULT", self.vault.clone().into_os_string()),
            ("ICLOUD_NOTES_SYNC_BIN", stub_engine().into_os_string()),
            ("ICLOUD_NOTES_SYNC_REAL", real_engine().into_os_string()),
            ("ICLOUD_NOTES_SYNC_STUB_LOG", dir("stub.log")),
        ]
    }

    /// The binary with this world's environment and nothing else, as the
    /// leader of a process group of its own (see `end_group`).
    fn command(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.env_clear().envs(self.environment()).process_group(0);
        c
    }

    /// `icloud-notes <args>` as one line for `sh`, in this world's
    /// environment and nothing else (for a run on a pseudo-terminal).
    pub fn shell_line(&self, args: &[&str]) -> String {
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let mut line = String::from("exec env -i");
        for (key, value) in self.environment() {
            line += &format!(" {key}={}", quote(value.to_str().unwrap()));
        }
        line += &format!(" {}", quote(self.bin.to_str().unwrap()));
        for arg in args {
            line += &format!(" {}", quote(arg));
        }
        line
    }

    /// Starts the binary and leaves it running (the window, which never
    /// exits by itself) until what is returned is dropped.
    pub fn spawn<I, S>(&self, args: I) -> Started
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let child = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the binary starts");
        Started(child)
    }
}

/// Ends a started binary with everything it started in turn (the engine, or
/// the window a launcher hands over to): the process group it leads, which
/// holds nothing else.
fn end_group(child: &mut Child) {
    let group = format!("-{}", child.id());
    let _ = Command::new("kill")
        .args(["-KILL", "--", &group])
        .stderr(Stdio::null())
        .status();
    let _ = child.wait();
}

/// A binary left running, ended when dropped.
pub struct Started(Child);

impl Started {
    pub fn exited(&mut self) -> bool {
        self.0.try_wait().unwrap().is_some()
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        end_group(&mut self.0);
    }
}

pub struct Cmd<'w> {
    world: &'w World,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    default_vault: bool,
    stdin: Option<Vec<u8>>,
}

impl Cmd<'_> {
    /// No `ICLOUD_NOTES_VAULT`: the vault is the default one, under the
    /// world's scratch home (`<home>/Documents`).
    pub fn default_vault(mut self) -> Self {
        self.default_vault = true;
        self
    }

    pub fn env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.env.push((key.into(), value.as_ref().to_os_string()));
        self
    }

    /// The real engine instead of the stub, answering from a recorded iCloud
    /// conversation with its clock and identifiers pinned.
    pub fn replay(self, cassette: &Path) -> Self {
        self.env("ICLOUD_NOTES_SYNC_BIN", real_engine())
            .env("ICLOUD_NOTES_SYNC_CASSETTE", cassette)
            .env("ICLOUD_NOTES_SYNC_NOW", "1790000000000")
            .env("ICLOUD_NOTES_SYNC_DETERMINISTIC", "1")
    }

    pub fn stdin(mut self, text: impl AsRef<[u8]>) -> Self {
        self.stdin = Some(text.as_ref().to_vec());
        self
    }

    pub fn json(mut self) -> Self {
        self.args.insert(0, "--json".into());
        self
    }

    pub fn styled(mut self) -> Self {
        self.args.insert(0, "--styled".into());
        self
    }

    pub fn run(self) -> Run {
        let mut command = self.world.command();
        if self.default_vault {
            command.env_remove("ICLOUD_NOTES_VAULT");
        }
        command
            .args(&self.args)
            .envs(self.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let started = Instant::now();
        let mut child = command.spawn().expect("the binary starts");
        if let Some(text) = self.stdin {
            child.stdin.take().unwrap().write_all(&text).unwrap();
        }
        let drain = |mut pipe: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).unwrap();
                String::from_utf8(bytes).expect("UTF-8 output")
            })
        };
        let out = drain(Box::new(child.stdout.take().unwrap()));
        let err = drain(Box::new(child.stderr.take().unwrap()));
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > RUN_LIMIT {
                end_group(&mut child);
                panic!("icloud-notes {:?} did not exit (did it open the window?)", self.args);
            }
            thread::sleep(Duration::from_millis(2));
        };
        Run {
            args: self.args,
            code: status.code().unwrap_or(-1),
            out: out.join().unwrap(),
            err: err.join().unwrap(),
            elapsed: started.elapsed(),
        }
    }
}

/// What one run printed and how it ended.
#[derive(Debug)]
pub struct Run {
    #[expect(dead_code, reason = "read through Debug, in a failed assertion's message")]
    args: Vec<OsString>,
    pub code: i32,
    pub out: String,
    pub err: String,
    pub elapsed: Duration,
}

impl Run {
    /// stdout as one JSON document.
    pub fn doc(&self) -> Value {
        serde_json::from_str(&self.out).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {self:#?}"))
    }

    /// The result of a run that worked, read from the envelope
    /// `{"ok":true,"data","summary"}`: `ok` first, exactly those keys.
    pub fn data(&self) -> Value {
        let doc = self.doc();
        assert!(self.out.starts_with("{\n    \"ok\": true,"), "{self:#?}");
        let keys: Vec<&String> = doc.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["data", "ok", "summary"], "{self:#?}");
        doc["data"].clone()
    }

    pub fn summary(&self) -> String {
        self.doc()["summary"]
            .as_str()
            .unwrap_or_else(|| panic!("no summary: {self:#?}"))
            .to_string()
    }

    /// Asserts a run that worked (exit 0, nothing on stderr) and gives its data.
    #[track_caller]
    pub fn ok(&self) -> Value {
        assert_eq!((self.code, self.err.as_str()), (0, ""), "{self:#?}");
        self.data()
    }

    /// Asserts the contract's machine failure: exit `exit`, stderr empty, and
    /// on stdout one document `{"ok":false,"error":{"code","message","hint"?}}`
    /// with `ok` first. Gives `(message, hint)`, the hint "" when left out.
    #[track_caller]
    pub fn fails(&self, exit: i32, code: &str) -> (String, String) {
        assert_eq!((self.code, self.err.as_str()), (exit, ""), "{self:#?}");
        assert!(self.out.starts_with("{\n    \"ok\": false,"), "{self:#?}");
        let doc = self.doc();
        let keys: Vec<&String> = doc.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["error", "ok"], "{self:#?}");
        let error = doc["error"].as_object().unwrap();
        assert_eq!(error["code"], code, "{self:#?}");
        assert!(
            error.keys().all(|k| ["code", "message", "hint"].contains(&k.as_str())),
            "{self:#?}"
        );
        // A hint is left out, never printed empty.
        let hint = error.get("hint").map(|h| h.as_str().unwrap().to_string());
        assert_ne!(hint.as_deref(), Some(""), "{self:#?}");
        (error["message"].as_str().unwrap().to_string(), hint.unwrap_or_default())
    }

    /// Asserts a text-mode failure: exit `exit`, stdout empty, `error:
    /// MESSAGE` and, when `hint` is not empty, `hint: HINT` on stderr.
    #[track_caller]
    pub fn fails_in_text(&self, exit: i32, message: &str, hint: &str) {
        let hint = if hint.is_empty() {
            String::new()
        } else {
            format!("hint: {hint}\n")
        };
        assert_eq!(
            (self.code, self.out.as_str(), self.err.as_str()),
            (exit, "", format!("error: {message}\n{hint}").as_str()),
            "{self:#?}"
        );
    }
}

/// The strings of a JSON array.
pub fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {value}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// The `key` of each object in a JSON array.
pub fn column(value: &Value, key: &str) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {value}"))
        .iter()
        .map(|v| v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")).to_string())
        .collect()
}

/// Compares `actual` with the committed text `fixtures/<name>` (text a port
/// must reproduce to the byte). `ICLOUD_NOTES_BLACKBOX_REGEN=1` writes it
/// instead, from the binary under test.
#[track_caller]
pub fn fixture(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/blackbox/fixtures")
        .join(name);
    if std::env::var_os("ICLOUD_NOTES_BLACKBOX_REGEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(actual, expected, "differs from {}", path.display());
}

/// Milliseconds since the epoch as the binary prints a time: UTC, ISO 8601,
/// to the second.
pub fn iso(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let (days, clock) = (seconds.div_euclid(86400), seconds.rem_euclid(86400));
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    )
}
