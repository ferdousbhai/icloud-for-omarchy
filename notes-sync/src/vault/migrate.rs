//! Forward vault migrations (layout 2 → 3). Ports icloud-md
//! `src/notes/vaultMigrations.ts`.
//!
//! A vault is migrated up to [`CURRENT_LAYOUT_VERSION`] the moment a command
//! touches it; layout 1 (no `layoutVersion`) is refused, and anything newer
//! than 3 is refused untouched (this port never bumps the version). Each
//! migration is idempotent; the version bump written after it is the commit
//! point.

use std::path::Path;

use serde_json::Value;

use super::state::{CURRENT_LAYOUT_VERSION, RawStateFile, read_clone_state, read_raw_state_file, write_raw_state_file};
use super::state::{CloneState, node_display};
use crate::cmd::errors::Error;
use crate::md::frontmatter::{SplitOptions, join_frontmatter, set_note_id, split_frontmatter};

/// `OLDEST_MIGRATABLE_LAYOUT_VERSION`.
pub const OLDEST_MIGRATABLE_LAYOUT_VERSION: u64 = 2;

/// `VaultMigration`: `run` performs the on-disk work and returns the updated
/// state; the runner stamps `to` and writes it.
pub struct VaultMigration<'a> {
    pub from: u64,
    pub to: u64,
    pub describe: &'static str,
    #[allow(clippy::type_complexity)]
    pub run: Box<dyn Fn(&Path, RawStateFile) -> Result<RawStateFile, Error> + 'a>,
}

/// `recordTitleModeAndIds` (2 → 3): stamp every tracked note file that is on
/// disk with its `apple-note-id`, and record `titleMode` explicitly.
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

/// `VAULT_MIGRATIONS`.
pub fn vault_migrations() -> Vec<VaultMigration<'static>> {
    vec![VaultMigration {
        from: 2,
        to: 3,
        describe: "recording note ids in each file's frontmatter",
        run: Box::new(record_title_mode_and_ids),
    }]
}

/// `migrationReporter`'s line for one migration.
pub fn migration_status(migration: &VaultMigration<'_>) -> String {
    format!("Updating this vault's layout: {}...", migration.describe)
}

/// `runVaultMigrations`: `Ok(false)` when `target_dir` isn't a clone.
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
                "No vault migration registered from layout version {version} to {target_version} - this is a bug in icloud-md."
            )));
        };
        on_migration(migration);
        state = (migration.run)(target_dir, state)?;
        state.insert("layoutVersion".into(), migration.to.into());
        write_raw_state_file(target_dir, &state)?;
        version = migration.to as f64;
    }
    Ok(true)
}

/// `openVault`: migrate forward if needed, then `read_clone_state`.
/// `on_status` receives migration progress lines (`migrationReporter`).
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
