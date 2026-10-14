//! Installs this CLI's agent skill for coding agents, following the hey-cli
//! convention: the skill is embedded in the binary and copied to
//! `~/.agents/skills/<name>/` (read by Codex, Pi and others), and Claude Code
//! gets a `~/.claude/skills/<name>` link to it. Only directories this CLI
//! created (marked by `.managed-by-<cli>`) are ever written, refreshed or
//! removed; a symlink or a hand-authored skill at that path is left alone.
//! The same module is copied into each of our products' CLIs; keep them in sync.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct Skill {
    /// Skill directory name, e.g. `cloud-calendar`.
    pub name: &'static str,
    /// Embedded SKILL.md.
    pub body: &'static str,
    /// The CLI that manages it, used in the ownership marker name.
    pub cli: &'static str,
    /// The CLI version, stamped so a new release refreshes the copy.
    pub version: &'static str,
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// Files and links written or removed.
    pub paths: Vec<PathBuf>,
    /// Entries left alone, and why.
    pub notices: Vec<String>,
}

const SKILL_FILE: &str = "SKILL.md";
const VERSION_FILE: &str = ".installed-version";

impl Skill {
    fn marker(&self) -> String {
        format!(".managed-by-{}", self.cli)
    }
    fn dir(&self, home: &Path) -> PathBuf {
        home.join(".agents/skills").join(self.name)
    }
    fn claude_link(&self, home: &Path) -> PathBuf {
        home.join(".claude/skills").join(self.name)
    }
    fn claude_target(&self) -> PathBuf {
        Path::new("../../.agents/skills").join(self.name)
    }
    /// True when `dir` is a real directory carrying this CLI's marker file.
    fn owns(&self, dir: &Path) -> bool {
        let is_dir = fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir());
        is_dir && fs::symlink_metadata(dir.join(self.marker())).is_ok_and(|m| m.is_file())
    }
}

/// `$HOME`, if set and non-empty.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn unmanaged(path: &Path, cli: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} exists but was not written by {cli}; move it aside to let {cli} install its skill there", path.display()),
    )
}

/// Writes a regular file, refusing to follow a symlink or replace anything else.
fn write_file(path: &Path, data: &str, cli: &str) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if !m.is_file() => return Err(unmanaged(path, cli)),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    fs::write(path, data)
}

/// Installs or updates the skill. Fails without writing when the skill
/// directory is a symlink, a file, or a non-empty directory this CLI did not create.
pub fn install(home: &Path, skill: &Skill) -> io::Result<Outcome> {
    let dir = skill.dir(home);
    match fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir_all(&dir)?,
        Err(e) => return Err(e),
        Ok(m) if !m.is_dir() => return Err(unmanaged(&dir, skill.cli)),
        Ok(_) if !skill.owns(&dir) && fs::read_dir(&dir)?.next().is_some() => {
            return Err(unmanaged(&dir, skill.cli));
        }
        Ok(_) => {}
    }
    let marker = format!("This skill is managed by {}. Manual edits will be overwritten on upgrade.\n", skill.cli);
    write_file(&dir.join(skill.marker()), &marker, skill.cli)?;
    write_file(&dir.join(SKILL_FILE), skill.body, skill.cli)?;
    write_file(&dir.join(VERSION_FILE), skill.version, skill.cli)?;
    let mut out = Outcome { paths: vec![dir.join(SKILL_FILE)], notices: vec![] };

    if fs::symlink_metadata(home.join(".claude")).is_ok_and(|m| m.is_dir()) {
        let link = skill.claude_link(home);
        let target = skill.claude_target();
        match fs::symlink_metadata(&link) {
            Ok(m) if m.file_type().is_symlink() && fs::read_link(&link)? == target => {}
            // A dangling link points nowhere useful: replace it.
            Ok(m) if m.file_type().is_symlink() && !link.exists() => {
                fs::remove_file(&link)?;
                symlink(&target, &link)?;
                out.paths.push(link);
            }
            Ok(_) => out.notices.push(format!("Left {} alone: it is not a link {} made.", link.display(), skill.cli)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(home.join(".claude/skills"))?;
                symlink(&target, &link)?;
                out.paths.push(link);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Removes the skill and the Claude Code link, but only what this CLI created.
pub fn remove(home: &Path, skill: &Skill) -> io::Result<Outcome> {
    let mut out = Outcome::default();
    let link = skill.claude_link(home);
    if fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) && fs::read_link(&link)? == skill.claude_target() {
        fs::remove_file(&link)?;
        out.paths.push(link);
    }
    let dir = skill.dir(home);
    if skill.owns(&dir) {
        for f in [SKILL_FILE, VERSION_FILE, &skill.marker()] {
            match fs::remove_file(dir.join(f)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        if fs::remove_dir(&dir).is_err() {
            out.notices.push(format!("Left {}: it holds files {} did not write.", dir.display(), skill.cli));
        }
        out.paths.push(dir);
    } else if fs::symlink_metadata(&dir).is_ok() {
        out.notices.push(format!("Left {} alone: it is not a skill {} installed.", dir.display(), skill.cli));
    }
    Ok(out)
}

/// Brings an installed copy up to date after an upgrade. Silent and cheap:
/// does nothing unless this CLI owns the copy and its stamped version differs.
pub fn refresh(home: &Path, skill: &Skill) -> bool {
    let dir = skill.dir(home);
    if !skill.owns(&dir) || fs::read_to_string(dir.join(VERSION_FILE)).is_ok_and(|v| v == skill.version) {
        return false;
    }
    write_file(&dir.join(SKILL_FILE), skill.body, skill.cli).is_ok()
        && write_file(&dir.join(VERSION_FILE), skill.version, skill.cli).is_ok()
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "linking the skill for Claude Code needs a Unix system"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Skill = Skill { name: "demo", body: "# demo v2\n", cli: "demo", version: "2" };

    fn home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("agent-skill-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&h);
        fs::create_dir_all(h.join(".claude")).unwrap();
        h
    }

    #[test]
    fn installs_links_refreshes_and_removes() {
        let h = home("cycle");
        let out = install(&h, &S).unwrap();
        assert_eq!(fs::read_to_string(h.join(".agents/skills/demo/SKILL.md")).unwrap(), S.body);
        assert_eq!(fs::read_link(h.join(".claude/skills/demo")).unwrap(), Path::new("../../.agents/skills/demo"));
        assert_eq!(fs::read_to_string(h.join(".claude/skills/demo/SKILL.md")).unwrap(), S.body);
        assert_eq!(out.paths.len(), 2);
        install(&h, &S).unwrap(); // idempotent

        let old = Skill { body: "# demo v1\n", version: "1", ..S };
        install(&h, &old).unwrap();
        assert!(refresh(&h, &S));
        assert!(!refresh(&h, &S));
        assert_eq!(fs::read_to_string(h.join(".agents/skills/demo/SKILL.md")).unwrap(), S.body);

        remove(&h, &S).unwrap();
        assert!(fs::symlink_metadata(h.join(".agents/skills/demo")).is_err());
        assert!(fs::symlink_metadata(h.join(".claude/skills/demo")).is_err());
        fs::remove_dir_all(&h).unwrap();
    }

    #[test]
    fn never_touches_what_it_did_not_create() {
        let h = home("foreign");
        // A symlinked skill (e.g. a source checkout): never written through.
        let elsewhere = h.join("checkout");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("SKILL.md"), "mine").unwrap();
        fs::create_dir_all(h.join(".agents/skills")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, h.join(".agents/skills/demo")).unwrap();
        assert!(install(&h, &S).is_err());
        assert!(!refresh(&h, &S));
        remove(&h, &S).unwrap();
        assert_eq!(fs::read_to_string(elsewhere.join("SKILL.md")).unwrap(), "mine");

        // A hand-authored directory: left alone, as is a foreign Claude entry.
        fs::remove_file(h.join(".agents/skills/demo")).unwrap();
        fs::create_dir_all(h.join(".agents/skills/demo")).unwrap();
        fs::write(h.join(".agents/skills/demo/SKILL.md"), "mine").unwrap();
        assert!(install(&h, &S).is_err());
        assert_eq!(fs::read_to_string(h.join(".agents/skills/demo/SKILL.md")).unwrap(), "mine");
        fs::remove_dir_all(h.join(".agents/skills/demo")).unwrap();
        fs::create_dir_all(h.join(".claude/skills/demo")).unwrap();
        let out = install(&h, &S).unwrap();
        assert_eq!(out.notices.len(), 1);
        remove(&h, &S).unwrap();
        assert!(h.join(".claude/skills/demo").is_dir());
        fs::remove_dir_all(&h).unwrap();
    }
}
