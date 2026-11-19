//! The black-box suite: `icloud-notes` driven as a separate process, the way
//! an agent or the systemd timer runs it, through nothing but its public
//! surface (arguments, stdin, stdout, stderr, the exit code, the files of a
//! scratch vault, the lock file, the engine it starts and the icloud-session
//! it asks). `ICLOUD_NOTES_BIN` names the binary, so the same checks hold the
//! Qt app and a port of it to the same behaviour; without it every test
//! passes having run nothing (`cargo test --workspace` has no binary to
//! drive: notes/bin/test builds the Qt one and runs the suite on it).
//!
//! Each test has a world of its own (world.rs): a temporary home, vault,
//! trash and D-Bus session bus, the fake icloud-session (session.rs) and the
//! repository's stub engine. Never the owner's notes, account or daemon.
//!
//! JSON is compared parsed, apart from the leading `"ok"` the contract
//! promises: layout and key order are not part of it. What only the window
//! reaches (the merge of unsaved edits, the sidebar tree) is pinned by
//! tests/fixtures/syncmodel.json instead, dumped from the C++ by
//! tests/syncmodel_fixtures.cpp.

mod session;
mod world;

mod background;
mod changing;
mod conflicts;
mod entry;
mod left;
mod location;
mod lock;
mod output;
mod passthrough;
mod reading;
mod real_engine;
mod sync;
mod terminal;
mod window;
