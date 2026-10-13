# icloud-for-omarchy

Native iCloud apps for Omarchy/Arch — Notes, Photos, Find My, Reminders, Contacts — sharing one
Apple web sign-in held by the `icloud-session` D-Bus daemon. Each app is a GUI plus a
scriptable `--json` CLI.

This file is for working on the code. For *using* the CLIs as an agent, read
[docs/AGENTS.md](docs/AGENTS.md) (short) and [docs/CLI.md](docs/CLI.md) (every command, JSON shape
and error code); `docs/skills/icloud/SKILL.md` is the installable skill.

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
- `packaging/PKGBUILD` — one split recipe for all six packages, built and tested together. `install.sh` — the signed-repo one-liner installer.
- `bin/` — `build`, `test`, `build-packages <arch>` (native Arch build), `arch-run` (disposable CI/verification chroot), `collect-packages`,
  `make-installers`, `release`, `verify-release`, `dev-install`/`dev-uninstall` (puts a release
  sessiond in `~/.local/bin` with a user D-Bus activation file).
- `tests/` — installer tests (`install_test.sh`, stubbed pacman in a user namespace),
  `packaging_test.py`, and the pinned hash of `add_signed_repo`.

## Commands

```sh
bin/test                      # rustfmt check, clippy -D warnings, all Rust tests, notes/bin/test, installer + packaging tests
bin/build [icloud-photos ...] # release binaries into target/release/, Notes into notes/build/
cargo test -p icloud-findmy   # one crate
cargo fmt --all               # max_width = 120 (rustfmt.toml)
bin/build-packages "$(uname -m)" # native Arch packages; no Docker
```

Build deps: rust, sqlite, gtk4, libadwaita, libshumate (findmy), webkitgtk-6.0 (sign-in window;
without it `bin/test` tests sessiond with `--no-default-features`), qt6-base, qt6-declarative,
make. GitHub's Ubuntu runners use `bin/arch-run` with `arch-install-scripts` to build
in a disposable native Arch chroot; local package builds run directly as a regular
Arch user. Release installer checks also use fresh chroots, never the host.
CI (`.github/workflows/packages.yml`) runs fmt, `bash -n` on the scripts, the installer and
packaging tests, then native x86_64/aarch64 package builds.

## Conventions and gotchas

- CLI contract, shared by every tool: `--json` puts only the result on stdout; failures end
  stderr with `{"error":{"code","message","exit_code","hint"}}`; exit codes 0/1/2 (sign-in)/
  3 (has changes)/4 (Find My password)/64 (usage or destructive command without `--yes`). Nothing
  prompts without a TTY. New commands must follow this and be added to docs/CLI.md, docs/AGENTS.md
  and the skill.
- No app signs in by itself: everything goes through icloud-sessiond. Tests never touch the real
  account; use the fakes.
- One suite version in `[workspace.package]` of `Cargo.toml` is inherited by every Rust crate
  and package. `bin/release <version>` tests, tags `v<version>`, dispatches native CI and signs
  locally; published-install verification runs only in CI. Do not release unless asked.
  Versions must increase from the 0.8.0 baseline; historical per-app tags remain untouched.
  crates.io publication is a separate explicit `cargo publish --locked -p icloud-session`.
- The installer pins `add_signed_repo` in `tests/add_signed_repo.sha256`; update the hash
  deliberately when changing the function. Ghost has removed its old copy.
- `/target`, `/dist` and makepkg dirs are gitignored build output.
