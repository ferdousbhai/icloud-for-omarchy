//! Forward vault migrations (layout 2 → 3). Ports icloud-md
//! `src/notes/vaultMigrations.ts`. Owner: workstream D.
#![allow(unused_variables)]

use std::path::Path;

use super::state::CloneState;
use crate::cmd::errors::Error;

/// `openVault`: migrate forward if needed, then `read_clone_state`.
/// `on_status` receives migration progress lines (`migrationReporter`).
pub fn open_vault(target_dir: &Path, on_status: &mut dyn FnMut(&str)) -> Result<Option<CloneState>, Error> {
    todo!()
}
