//! Forward vault migrations (layout 2 → 3 → 4). The runner is originally
//! derived from icloud-md.
//!
//! Only commands that take the vault lock (clone, pull, push, restore, sync)
//! migrate, through [`open_vault`]/[`require_vault`]; the read-only ones
//! (status, push --dry-run, history, diff, vault-info) read a layout 3 vault
//! where it is ([`read_vault`]). Layout 1 (no `layoutVersion`) is refused,
//! and anything newer than [`CURRENT_LAYOUT_VERSION`] is refused untouched.
//! Each migration is idempotent; the version bump written after it is the
//! commit point.

use std::path::Path;

use serde_json::Value;

use super::state::{
    CURRENT_LAYOUT_VERSION, CloneState, LEGACY_STATE_DIR_NAME, RawStateFile, STATE_DIR_NAME, STATE_FILE_NAME,
    STATE_SUBDIR_NAMES, node_display, read_clone_state, read_raw_state_file, to_js_json, write_atomic,
    write_raw_state_file,
};
use crate::cmd::errors::Error;
use crate::md::frontmatter::{SplitOptions, join_frontmatter, set_note_id, split_frontmatter};

/// The oldest layout a migration starts from.
const OLDEST_MIGRATABLE_LAYOUT_VERSION: u64 = 2;

/// What a backup's name carries between `.icloud-md` and its timestamp.
pub const BACKUP_INFIX: &str = ".bak-";

/// A migration: `run` performs the on-disk work and returns the updated
/// state; the runner stamps `to` and writes it.
pub struct VaultMigration<'a> {
    pub from: u64,
    pub to: u64,
    pub describe: &'static str,
    #[allow(clippy::type_complexity)]
    pub run: Box<dyn Fn(&Path, RawStateFile) -> Result<RawStateFile, Error> + 'a>,
}

/// 2 → 3: stamp every tracked note file that is on disk with its
/// `apple-note-id`, and record `titleMode` explicitly.
fn record_title_mode_and_ids(target_dir: &Path, mut state: RawStateFile) -> Result<RawStateFile, Error> {
    if let Some(Value::Object(notes)) = state.get("notes") {
        for (record_name, entry) in notes {
            let Some(file) = entry.get("file").and_then(Value::as_str) else {
                continue;
            };
            let path = target_dir.join(file);
            let existing = match std::fs::read(&path) {
                Ok(bytes) => super::local::decode_utf8(&bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let envelope = split_frontmatter(&existing, SplitOptions::default());
            let stamped = join_frontmatter(&set_note_id(&envelope.frontmatter, record_name), &envelope.body);
            if stamped != existing {
                std::fs::write(&path, stamped)?;
            }
        }
    }
    let mode = if state.get("titleMode").and_then(Value::as_str) == Some("filename") {
        "filename"
    } else {
        "in-body"
    };
    state.insert("titleMode".into(), mode.into());
    Ok(state)
}

/// 3 → 4: move the state from `.icloud-md/` to `.icloud-notes/`.
///
/// Each step is idempotent, so a run interrupted anywhere picks up where it
/// stopped: back `.icloud-md/` up (a copy, `.icloud-md.bak-<UTC time>`, kept
/// for the user to delete) unless `.icloud-notes/` already exists (an
/// earlier attempt got past this); create `.icloud-notes/`; rename base/,
/// history/ and conflict-backups/ into it. The runner then writes the new
/// state.json there (temp file, then rename: the commit point), and
/// [`finish_layout_4`] turns `.icloud-md/state.json` into a tombstone. Note
/// files are never touched.
fn move_state_dir(target_dir: &Path, state: RawStateFile) -> Result<RawStateFile, Error> {
    let legacy = target_dir.join(LEGACY_STATE_DIR_NAME);
    let current = target_dir.join(STATE_DIR_NAME);
    if !current.exists() {
        back_up_legacy_state_dir(target_dir)?;
        std::fs::create_dir_all(&current)?;
    }
    for name in STATE_SUBDIR_NAMES {
        move_merging(&legacy.join(name), &current.join(name))?;
    }
    Ok(state)
}

/// Copy `.icloud-md/` to `.icloud-md.bak-<UTC time>`, through a `.partial`
/// directory renamed into place, so a backup that exists is complete.
fn back_up_legacy_state_dir(target_dir: &Path) -> Result<(), Error> {
    let legacy = target_dir.join(LEGACY_STATE_DIR_NAME);
    if !legacy.is_dir() {
        return Ok(());
    }
    let prefix = format!("{LEGACY_STATE_DIR_NAME}{BACKUP_INFIX}");
    // An earlier attempt's unfinished copy.
    for entry in std::fs::read_dir(target_dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".partial") {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    // 2026-10-05T12:34:56.789Z → 20261005T123456Z
    let iso = crate::js::iso_string(super::rt::now_ms());
    let seconds = iso.split('.').next().unwrap_or(&iso);
    let stamp: String = seconds.chars().filter(|c| !matches!(c, '-' | ':')).collect();
    let mut name = format!("{prefix}{stamp}Z");
    for n in 2.. {
        if !target_dir.join(&name).exists() {
            break;
        }
        name = format!("{prefix}{stamp}Z-{n}");
    }
    let partial = target_dir.join(format!("{name}.partial"));
    copy_dir_all(&legacy, &partial)?;
    std::fs::rename(&partial, target_dir.join(&name))?;
    Ok(())
}

fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Rename `from` to `to`; when both exist, move over what `to` lacks
/// (directories merged the same way) and leave what both have in `from`.
fn move_merging(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(from).is_err() {
        return Ok(());
    }
    if std::fs::symlink_metadata(to).is_err() {
        return std::fs::rename(from, to);
    }
    if !(from.is_dir() && to.is_dir()) {
        return Ok(());
    }
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        move_merging(&entry.path(), &to.join(entry.file_name()))?;
    }
    // Goes only when everything in it moved.
    let _ = std::fs::remove_dir(from);
    Ok(())
}

/// What `.icloud-md/state.json` says once the state has moved, so an older
/// engine or app refuses the vault as written by a newer version instead of
/// treating it as not cloned.
pub fn tombstone() -> RawStateFile {
    let mut map = RawStateFile::new();
    map.insert("layoutVersion".into(), CURRENT_LAYOUT_VERSION.into());
    map.insert("movedTo".into(), STATE_DIR_NAME.into());
    map
}

/// Once `.icloud-notes/state.json` exists: `.icloud-md/state.json`, if
/// there, becomes the tombstone. Run after the 3 → 4 commit and on every
/// later open under the lock, in case a run stopped right after the commit.
fn finish_layout_4(target_dir: &Path) -> Result<(), Error> {
    let path = target_dir.join(LEGACY_STATE_DIR_NAME).join(STATE_FILE_NAME);
    if !path.exists() || !target_dir.join(STATE_DIR_NAME).join(STATE_FILE_NAME).exists() {
        return Ok(());
    }
    let tombstone = to_js_json(&Value::Object(tombstone()));
    if std::fs::read(&path).is_ok_and(|on_disk| on_disk == tombstone.as_bytes()) {
        return Ok(());
    }
    write_atomic(&path, tombstone.as_bytes())?;
    Ok(())
}

/// The migrations, in order.
pub fn vault_migrations() -> Vec<VaultMigration<'static>> {
    vec![
        VaultMigration {
            from: 2,
            to: 3,
            describe: "recording note ids in each file's frontmatter",
            run: Box::new(record_title_mode_and_ids),
        },
        VaultMigration {
            from: 3,
            to: 4,
            describe: "moving its state from .icloud-md/ to .icloud-notes/",
            run: Box::new(move_state_dir),
        },
    ]
}

/// The status line for one migration.
fn migration_status(migration: &VaultMigration<'_>) -> String {
    format!("Updating this vault's layout: {}...", migration.describe)
}

/// Run `migrations` up to `target_version`: `Ok(false)` when `target_dir`
/// isn't a clone.
pub fn run_vault_migrations(
    target_dir: &Path,
    migrations: &[VaultMigration<'_>],
    target_version: u64,
    on_migration: &mut dyn FnMut(&VaultMigration<'_>),
) -> Result<bool, Error> {
    let Some(raw) = read_raw_state_file(target_dir)? else {
        return Ok(false);
    };
    let mut version = raw.get("layoutVersion").and_then(Value::as_f64).unwrap_or(0.0);
    if version < OLDEST_MIGRATABLE_LAYOUT_VERSION as f64 {
        return Err(Error::UnsupportedVaultLayout {
            target_dir: node_display(target_dir),
        });
    }
    if version > target_version as f64 {
        return Err(Error::VaultFromNewerTool {
            target_dir: node_display(target_dir),
            vault_version: version as u64,
            supported_version: target_version as u32,
        });
    }

    let mut state = raw;
    while version < target_version as f64 {
        let Some(migration) = migrations.iter().find(|m| m.from as f64 == version) else {
            return Err(Error::Internal(format!(
                "No vault migration registered from layout version {version} to {target_version} - this is a bug in icloud-notes-sync."
            )));
        };
        on_migration(migration);
        state = (migration.run)(target_dir, state)?;
        state.insert("layoutVersion".into(), migration.to.into());
        write_raw_state_file(target_dir, &state)?;
        version = migration.to as f64;
    }
    if version >= f64::from(CURRENT_LAYOUT_VERSION) {
        finish_layout_4(target_dir)?;
    }
    Ok(true)
}

/// For a command holding the vault lock: migrate forward if needed, then
/// [`read_clone_state`]. `on_status` receives migration progress lines.
pub fn open_vault(target_dir: &Path, on_status: &mut dyn FnMut(&str)) -> Result<Option<CloneState>, Error> {
    let migrations = vault_migrations();
    let is_clone = run_vault_migrations(target_dir, &migrations, u64::from(CURRENT_LAYOUT_VERSION), &mut |m| {
        on_status(&migration_status(m));
    })?;
    if !is_clone {
        return Ok(None);
    }
    read_clone_state(target_dir)
}

/// [`open_vault`] for a command that needs a clone: none is
/// `NotClonedDirectory`.
pub fn require_vault(target_dir: &Path, on_status: &mut dyn FnMut(&str)) -> Result<CloneState, Error> {
    open_vault(target_dir, on_status)?.ok_or_else(|| not_cloned(target_dir))
}

/// For a read-only command: the vault's state as it is, never migrated (a
/// layout 3 vault is read from `.icloud-md/`). None is `NotClonedDirectory`.
pub fn read_vault(target_dir: &Path) -> Result<CloneState, Error> {
    read_clone_state(target_dir)?.ok_or_else(|| not_cloned(target_dir))
}

fn not_cloned(target_dir: &Path) -> Error {
    Error::NotClonedDirectory {
        target_dir: target_dir.display().to_string(),
    }
}
