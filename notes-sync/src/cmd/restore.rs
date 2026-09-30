//! `restore`. Ports icloud-md `src/commands/restore.ts`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Error;
use crate::md::frontmatter::{join_frontmatter, split_frontmatter};
use crate::vault::base::read_base_copy;
use crate::vault::history::resolve_tracked_note;
use crate::vault::local::{read_text, split_options};
use crate::vault::migrate::open_vault;

/// `RestoreResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreResult {
    pub file: String,
}

/// `runRestore`: overwrite a tracked note's body with its base copy,
/// keeping the file's frontmatter. Purely local.
pub fn run_restore(target_dir: &Path, file: &str) -> Result<RestoreResult, Error> {
    let Some(state) = open_vault(target_dir, &mut |_| {})? else {
        return Err(Error::NotClonedDirectory {
            target_dir: target_dir.display().to_string(),
        });
    };
    let tracked = resolve_tracked_note(&state, file, target_dir)?;
    let Some(base) = read_base_copy(target_dir, &tracked.record_name)? else {
        return Err(Error::Internal(format!(
            "\"{}\" has no base copy to restore to - this shouldn't happen for a tracked note.",
            tracked.entry.file
        )));
    };
    let path = target_dir.join(&tracked.entry.file);
    let existing = read_text(&path)?.unwrap_or_default();
    let envelope = split_frontmatter(&existing, split_options(state.mode()));
    std::fs::write(&path, join_frontmatter(&envelope.frontmatter, &base))?;
    Ok(RestoreResult {
        file: tracked.entry.file,
    })
}
