//! What the apps' command-line tests share (`<app>/tests/cli.rs`): running
//! the built binary against the app's fake server through icloud-session's
//! mock mode, reading what it printed, and the checks every app repeats
//! (the command line links no GTK; with no command it becomes the app).

use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

/// The binary `bin` as a test runs it: icloud-session's mock mode sending
/// every request to `base` (the fake server), stdin not a terminal,
/// `--data-dir <tmp>/data`, a home and `XDG_DATA_HOME` in `tmp` too (so a
/// mistake cannot reach the real data), and no session bus. Add the
/// command's arguments and run it.
pub fn command(bin: &str, base: &str, tmp: &Path) -> Command {
    let mut c = Command::new(bin);
    c.arg("--data-dir")
        .arg(tmp.join("data"))
        .env("ICLOUD_SESSION_MOCK", "1")
        .env("ICLOUD_SESSION_MOCK_URL", base)
        .env("HOME", tmp.join("home"))
        .env("XDG_DATA_HOME", tmp.join("xdg"))
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus")
        .stdin(Stdio::null());
    c
}

pub fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Parses the JSON envelope a command printed on stdout
/// (`{ok,data,summary}` or `{ok:false,error}`).
pub fn envelope(stdout: impl AsRef<[u8]>) -> Value {
    let stdout = stdout.as_ref();
    serde_json::from_slice(stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(stdout)))
}

/// Writes an executable script.
pub fn write_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    // A virtual address to its file offset, through the PT_LOAD segments.
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

/// The command-line binary `cli` needs none of the `toolkit` libraries
/// (`"libgtk-4."`, `"libadwaita-1."`, ...), so a command does not load them;
/// the window's binary `app` needs them all. `app` is `None` or missing in
/// a build without the `ui` feature.
pub fn assert_cli_links_no_gtk(cli: &str, app: Option<&str>, toolkit: &[&str]) {
    let ui = |libs: &[String]| libs.iter().filter(|l| toolkit.iter().any(|p| l.starts_with(p))).count();
    let libs = needed_libs(cli);
    assert!(libs.iter().any(|l| l.starts_with("libc.")), "{libs:?}");
    assert_eq!(ui(&libs), 0, "{libs:?}");
    if let Some(app) = app.filter(|p| Path::new(p).exists()) {
        assert_eq!(ui(&needed_libs(app)), toolkit.len());
    }
}

/// With no command, the command-line binary `cli` (named `name`) becomes
/// `<name>-app` from its own directory (the same process: its exit code is
/// the app's), and says so when there is none.
pub fn assert_no_command_runs_the_app(cli: &str, name: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join(name);
    std::fs::copy(cli, &copy).unwrap();
    let app = tmp.path().join(format!("{name}-app"));
    let marker = tmp.path().join("ran");
    write_script(
        &app,
        &format!("#!/bin/sh\necho \"$#\" > '{}'\nexit 7\n", marker.display()),
    );
    // A test thread forking while the copy was open for writing holds its
    // fd until that child execs: "Text file busy" (ETXTBSY) for a moment.
    let mut tries = 0;
    let out = loop {
        match Command::new(&copy).stdin(Stdio::null()).output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            r => break r.unwrap(),
        }
    };
    assert_eq!(code(&out), 7, "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "0");

    // Without it (and none on PATH) it says so.
    std::fs::remove_file(&app).unwrap();
    let out = Command::new(&copy)
        .env("PATH", tmp.path().join("nowhere"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("cannot open the app"), "{}", stderr(&out));
}
