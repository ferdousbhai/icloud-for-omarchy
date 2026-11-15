//! The vault's Markdown spelling (`md::spelling`), and its one-time
//! upgrade.
//!
//! The spelling a vault's files are in is the one of the build that last
//! wrote its state.json, which its `generator` names
//! (`icloud-notes-sync X.Y.Z (Markdown spelling N)`; none named is
//! spelling 1). A command that takes the vault lock upgrades a vault of an
//! older spelling before it reads any note ([`upgrade_vault_spelling`],
//! from `migrate::open_vault`): each tracked note's file and base copy are
//! rewritten so that this build reads them as the older one did, whether
//! the file was edited or not, so an edit made before the upgrade means
//! what it meant and the merge sees nothing else change. Files made later
//! (new notes, what a pull writes) are this build's and never rewritten.
//!
//! Crash safety: the notes to rewrite are first recorded in state.json
//! ([`PENDING_SPELLING_KEY`], each with a hash of its file and base copy as
//! they were), which also stamps the new generator; then each file and base
//! copy still as it was is rewritten; then the record is cleared. A run cut
//! short anywhere is finished by the next one, and nothing is rewritten
//! twice: a rewritten file no longer has its old hash.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::base::{read_base_copy, replace_base_copy};
use super::local::{read_text, split_options, write_note_file};
use super::state::{CloneState, read_raw_state_file, write_clone_state, write_raw_state_file};
use crate::cmd::errors::Error;
use crate::md::frontmatter::{join_frontmatter, split_frontmatter};
use crate::md::spelling::{MARKDOWN_SPELLING, upgrade_spelling};

/// The state.json key of an upgrade under way: `{recordName: {"file":
/// hash, "base": hash}}` of the files as they were (`null`: unchanged).
pub const PENDING_SPELLING_KEY: &str = "pendingMarkdownSpelling";

/// The Markdown spelling `generator` (a state.json's) names.
pub fn spelling_of_generator(generator: Option<&str>) -> u32 {
    generator
        .and_then(|g| g.split_once("(Markdown spelling ")?.1.strip_suffix(')')?.parse().ok())
        .unwrap_or(1)
}

/// FNV-1a over `text`, as hex: tells a file as it was from one rewritten.
fn text_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A tracked note's file text and base copy, when there.
fn note_texts(
    target_dir: &Path,
    state: &CloneState,
    record_name: &str,
) -> Result<(Option<String>, Option<String>), Error> {
    let Some(entry) = state.notes.get(record_name) else {
        return Ok((None, None));
    };
    Ok((
        read_text(&target_dir.join(&entry.file))?,
        read_base_copy(target_dir, record_name)?,
    ))
}

/// `file_text` (a whole note file) with its body upgraded.
fn upgraded_file(file_text: &str, state: &CloneState) -> Option<String> {
    let envelope = split_frontmatter(file_text, split_options(state.mode()));
    let body = upgrade_spelling(&envelope.body)?;
    Some(join_frontmatter(&envelope.frontmatter, &body))
}

/// Upgrade the vault at `target_dir` (holding its lock) to this build's
/// spelling if it is in an older one, or finish an upgrade a run cut short.
/// `state` is updated as written.
pub fn upgrade_vault_spelling(target_dir: &Path, state: &mut CloneState) -> Result<(), Error> {
    let pending = match state.extra.get(PENDING_SPELLING_KEY) {
        Some(Value::Object(pending)) => pending.clone(),
        _ if spelling_of_generator(state.generator.as_deref()) >= MARKDOWN_SPELLING => return Ok(()),
        _ => {
            let mut pending = Map::new();
            for record_name in state.notes.keys() {
                let (file, base) = note_texts(target_dir, state, record_name)?;
                let file_hash = file
                    .as_deref()
                    .filter(|f| upgraded_file(f, state).is_some())
                    .map(text_hash);
                let base_hash = base.as_deref().filter(|b| upgrade_spelling(b).is_some()).map(text_hash);
                if file_hash.is_some() || base_hash.is_some() {
                    pending.insert(record_name.clone(), json!({ "file": file_hash, "base": base_hash }));
                }
            }
            if pending.is_empty() {
                // Nothing to rewrite: the generator says so from now on
                // (state.json otherwise as it is).
                if let Some(mut raw) = read_raw_state_file(target_dir)? {
                    raw.insert("generator".into(), crate::GENERATOR.into());
                    write_raw_state_file(target_dir, &raw)?;
                }
                state.generator = Some(crate::GENERATOR.to_owned());
                return Ok(());
            }
            state
                .extra
                .insert(PENDING_SPELLING_KEY.into(), Value::Object(pending.clone()));
            write_clone_state(target_dir, state)?;
            crate::crash::point(crate::crash::SPELLING_UPGRADE);
            pending
        }
    };
    for (record_name, hashes) in &pending {
        let (file, base) = note_texts(target_dir, state, record_name)?;
        if let (Some(file), Some(hash)) = (file, hashes["file"].as_str())
            && text_hash(&file) == hash
            && let Some(upgraded) = upgraded_file(&file, state)
        {
            write_note_file(&target_dir.join(&state.notes[record_name].file), &upgraded)?;
        }
        if let (Some(base), Some(hash)) = (base, hashes["base"].as_str())
            && text_hash(&base) == hash
            && let Some(upgraded) = upgrade_spelling(&base)
        {
            replace_base_copy(target_dir, record_name, &upgraded)?;
        }
    }
    state.extra.remove(PENDING_SPELLING_KEY);
    write_clone_state(target_dir, state)?;
    state.generator = Some(crate::GENERATOR.to_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_names_this_builds_spelling() {
        assert_eq!(spelling_of_generator(Some(crate::GENERATOR)), MARKDOWN_SPELLING);
        assert_eq!(spelling_of_generator(Some("icloud-notes-sync 0.11.1")), 1);
        assert_eq!(spelling_of_generator(None), 1);
    }
}
