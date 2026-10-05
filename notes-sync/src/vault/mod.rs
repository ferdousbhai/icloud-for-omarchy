//! The vault on disk.
//!
//! Originally derived from icloud-md.
//!
//! | module | what |
//! |---|---|
//! | state | the state directory and state.json |
//! | migrate | layout migrations |
//! | base | last-synced bodies (merge bases) |
//! | history | version snapshots, tracked-file resolution |
//! | epoch | whole-note epochs |
//! | layout | folder tree and note placement |
//! | folders | folder reconciliation and creation |
//! | attachments | attachment files |
//! | pairing | note-id pairing, deferred renames |
//! | local | working-file state, file times, vault root |
//! | rt | clock and randomness (with the differential harness's hooks) |

pub mod attachments;
pub mod base;
pub mod epoch;
pub mod folders;
pub mod history;
pub mod layout;
pub mod local;
pub mod migrate;
pub mod pairing;
pub mod rt;
pub mod state;
