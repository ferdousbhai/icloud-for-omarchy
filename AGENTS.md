# icloud-for-omarchy

Native iCloud apps for Omarchy/Arch — Notes, Photos, Find My, Reminders, Contacts, iCloud Drive — sharing one
Apple web sign-in held by the `icloud-session` D-Bus daemon. Each app is a GUI plus a
scriptable `--json` CLI; iCloud Drive is a synced folder with a small control command.

This file is for working on the code. For *using* the CLIs as an agent, read
[docs/CLI.md](docs/CLI.md) (every command, JSON shape
and error code); `docs/skills/icloud/SKILL.md` is the installable skill, embedded in `icloud-session skill` (`sessiond/src/agent_skill.rs`, shared verbatim with our other product CLIs).

## Code index

Cargo workspace (`Cargo.toml`) plus one Qt app:

- `session/` — `icloud-session` client crate (`lib.rs`, `cli.rs`): talks to the daemon over
  D-Bus. Published separately to crates.io when needed and consumed by sibling repos
  (cloud-calendar pins it by git `rev`), so keep its API stable.
- `sessiond/` — `icloud-sessiond`: the daemon (`daemon.rs`), Apple auth (`apple.rs`), keyring
  (`secrets.rs`), cookies, the WebKitGTK sign-in window (`src/bin/signin.rs`, the default
  feature), D-Bus activation files in `data/`.
- `notes-sync/` — `icloud-notes-sync`, the Notes sync engine (Rust port of icloud-md): CloudKit
  (`cloudkit/`), Markdown (`md/`), vault (`vault/`), commands (`cmd/`). Recorded scenarios and
  golden corpora live in `tests/`; regenerate with `ICLOUD_NOTES_SYNC_REGEN=1 cargo test -p
  icloud-notes-sync`. Ships inside the icloud-notes package, not on its own.
- `notes/` — `icloud-notes`, the Qt6/QML app and CLI (C++ in `src/`, QML in `qml/`,
  `notes/bin/build`, `notes/bin/test`).
- `photos/`, `findmy/`, `reminders/`, `contacts/` — one crate per app: a library, a GTK-free CLI
  binary (`src/main.rs` + `cli.rs`), and the GTK/libadwaita window as a separate `*-app` binary
  (`src/bin/app/`; Photos keeps its UI in `src/ui/` + `app.rs`). Each has a fake Apple server
  under `examples/` (selected with `ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=…`) and CLI
  tests in `tests/`.
- `reminders/`, `contacts/` — `icloud-reminders` and `icloud-contacts`, built like Photos and
  Find My: a library, a GTK-free CLI (`src/main.rs` + `cli.rs`) and the window as `*-app`
  (`src/bin/app/`), each with a fake Apple server in `examples/`. Reminders speaks CloudKit
  (`cloudkit.rs`, container `com.apple.reminders`) and has the systemd timer that syncs and sends
  due-time notifications (`data/`, `notify.rs`), and `folder.rs` keeps the reminders as Markdown
  checklists in `~/Documents/Reminders` for what only carries files (Syncthing to a phone), from
  the same timer with no command to turn it on: read its header before changing how lines are matched to
  reminders or what a file may delete; Contacts speaks icloud.com's contacts service
  (`api.rs`, `vcard.rs`). They were away from 2026-10-13 to 2026-11-13 as the iCloud halves of
  Cloud Tasks and Cloud Contacts, which also showed Google; the owner brought them back so that
  iCloud is the one source of truth. Do not add a second provider beside iCloud here; another
  service may only be imported from.
- `drive/` — `icloud-drive`: keeps `~/iCloud Drive` in sync both ways. No window: a library
  (`remote.rs` the Drive web API behind a trait, `plan.rs` the pure sync rules, `engine.rs`,
  `state.rs`, `local.rs`), the `icloud-drive` command (`status`, `sync`, `start`, `stop`) and a
  systemd user service in `data/`, off until `icloud-drive start`. `tests/sync.rs` runs the engine
  against an in-memory Drive that mirrors what was measured of the real one; read the notes at the
  top of `remote.rs` before changing how changes are detected.
- `packaging/PKGBUILD` — one split recipe for all seven packages, built and tested together. `install.sh` — the signed-repo one-liner installer.
- `bin/` — `build`, `check`, `check-shared-support` (vendored copies shared with sibling repos; this repo holds the canonical checker and its test, `tests/test_shared_support.py`; see `SHARED_SUPPORT.md`), `build-packages <arch>` (native Arch build), `arch-run` (disposable CI/verification chroot), `collect-packages`,
  `make-installers`, `release`, `verify-release`, `dev-install`/`dev-uninstall` (puts a release
  sessiond in `~/.local/bin` with a user D-Bus activation file).
- `tests/` — installer tests (`install_test.sh`, stubbed pacman in a user namespace),
  `packaging_test.py`, and the pinned hash of `add_signed_repo`.

## Commands

```sh
bin/check                      # rustfmt check, clippy -D warnings, all Rust tests, notes/bin/test, installer + packaging tests
bin/build [icloud-photos ...] # release binaries into target/release/, Notes into notes/build/
cargo test -p icloud-findmy   # one crate
cargo fmt --all               # max_width = 120 (rustfmt.toml)
bin/build-packages "$(uname -m)" # native Arch packages; no Docker
```

Build deps: rust, sqlite, gtk4, libadwaita, libshumate (findmy), webkitgtk-6.0 (sign-in window;
without it `bin/check` tests sessiond with `--no-default-features`), qt6-base, qt6-declarative,
make. GitHub's Ubuntu runners use `bin/arch-run` with `arch-install-scripts` to build
in a disposable native Arch chroot; local package builds run directly as a regular
Arch user. Release installer checks also use fresh chroots, never the host.
CI (`.github/workflows/packages.yml`) runs fmt, the installer (which syntax-checks the scripts) and
packaging tests, then native x86_64/aarch64 package builds.

## Conventions and gotchas

- CLI contract, shared by every tool (Rust: `session/src/output.rs` — `OutputFlags`, `Response`,
  `CliError`, the shared hints and `session_failure`/`http_failure`; Notes mirrors it in
  `notes/src/cli.cpp`): text on a terminal, the envelope `{"ok":true,"data","summary","meta"?}`
  when piped or with `--json`; `--styled`, `--quiet` (data only), `--ids-only`, `--count`, combinable
  (ids-only > count > quiet > json > styled). Failures are `{"ok":false,"error":{"code","message",
  "hint"?}}` on stdout in machine modes, `error:`/`hint:` on stderr in text mode; remediation goes in
  `hint`. Data plus a non-zero exit (`Response::exit`) for partial failures and signed-out
  `status`. Exit codes 0 ok / 1 other failure / 2 usage, ambiguous, exists, or a destructive
  command without `--yes` / 3 not signed in, credentials rejected, or not set up (the JSON `code`
  tells them apart) / 4 not found / 5 a service unavailable (network, daemon, keyring, iCloud 5xx).
  `push --dry-run` and `diff` report their changes and exit 0. Nothing prompts outside text mode on
  a TTY. New commands must follow this and be added to docs/CLI.md and the skill.
- No app signs in by itself: everything goes through icloud-sessiond. Tests never touch the real
  account; use the fakes.
- One suite version in `[workspace.package]` of `Cargo.toml` is inherited by every Rust crate
  and package. `bin/release <version>` tests, tags `v<version>`, dispatches native CI and signs
  locally; published-install verification runs only in CI. Do not release unless asked.
  Versions must increase (`bin/release` refuses otherwise); historical per-app tags remain untouched.
  crates.io publication is a separate explicit `cargo publish --locked -p icloud-session`.
- The installer pins `add_signed_repo` in `tests/add_signed_repo.sha256`; update the hash
  deliberately when changing the function. Ghost has removed its old copy.
- `/target`, `/dist` and makepkg dirs are gitignored build output.
