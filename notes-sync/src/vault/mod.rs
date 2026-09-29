//! The vault on disk. Owner: workstream D (`src/vault/**`, `src/cmd/**`,
//! `src/main.rs`, `tests/cli_*`).
//!
//! | module | ports (icloud-md `src/notes/`) |
//! |---|---|
//! | state | cloneState |
//! | migrate | vaultMigrations |
//! | base | baseCopy |
//! | history | versionHistory, trackedFile |
//! | epoch | noteEpoch |
//! | layout | folderLayout, folderTree |
//! | folders | folderReconcile, folderCreate |
//! | attachments | attachmentSync |
//! | pairing | noteIdPairing, pendingRename |
//! | local | localFileState, noteTimestamps, `vaultRoot.ts` |

pub mod attachments;
pub mod base;
pub mod epoch;
pub mod folders;
pub mod history;
pub mod layout;
pub mod local;
pub mod migrate;
pub mod pairing;
pub mod state;
