# icloud-notes-sync

The sync engine inside the [icloud-notes](../notes/README.md) package: your
iCloud Notes as a folder of Markdown files, synced both ways. A Rust port of [icloud-md](https://github.com/coddingtonbear/icloud-md)
0.6.2 by Adam Coddington, using [icloud-session](../session) for the
Apple sign-in instead of a browser of its own.

Every command below is implemented: clone, pull, status, push (with
`--dry-run` and every refusal), history, diff and restore, over the note
codec (protobuf, the Notes document model, tables, attachments), the
Markdown renderer and parser, diff3 merging and the CloudKit client. The
tests are icloud-md's own test suites ported to Rust, golden corpora,
recorded CloudKit sessions replayed end to end, and recorded CLI scenarios
that require the same exit codes, output, requests and vault files as when
they were recorded. Parity with icloud-md was established against icloud-md
itself; the recordings now come from this crate, so it is free to diverge. A
live write test runs against a real account on request only (see below). Where it deliberately differs from
icloud-md 0.6.2 is in [docs/PORT_PLAN.md](docs/PORT_PLAN.md).

It is not a command of its own. The package installs it off PATH, at
`/usr/lib/icloud-notes/icloud-notes-sync`, and the Notes window, its
background sync and the `icloud-notes` command run it (`icloud-notes sync`,
`pull`, `push [--dry-run]`, `clone`, `history [--records]`, `diff`,
`restore`; see [docs/CLI.md](../docs/CLI.md)). Up to notes-sync-v0.2.0 it
was a package of its own, `icloud-notes-sync`; icloud-notes now replaces
it, and the engine is released with icloud-notes, not on its own.

Run directly, for development or debugging, it takes these commands (from a
checkout, `cargo run -q -p icloud-notes-sync -- <command>`, or the
installed `/usr/lib/icloud-notes/icloud-notes-sync <command>`):

```console
$ icloud-notes-sync clone --account <dsid> ~/Notes
$ icloud-notes-sync pull
$ icloud-notes-sync --json status
$ icloud-notes-sync push
$ icloud-notes-sync sync
$ icloud-notes-sync history <file>
$ icloud-notes-sync diff <file> <id>|<from>..<to>
$ icloud-notes-sync restore <file>
$ icloud-notes-sync --json vault-info
```

Exit codes: 0 ok, 1 error, 2 iCloud sign-in required, 3 `status`/`push
--dry-run` has entries or `diff` found differences, 64 usage, 70 internal
error, the table the other iCloud tools share ([docs/CLI.md](../docs/CLI.md)).
With `--json`, stdout carries only the JSON result, and an error is one line
on stderr: `{"error":{"code":"sign_in_required","message":…,"exit_code":2,"hint":…}}`
(`code` is the error's class in snake case: `untracked_file`,
`not_cloned_directory`, `usage`, `internal`, …). icloud-md used 2 for usage,
1 for a sign-in and `{"error":"<Class>Error","exitCode":…}`.

`sync` is this port's own, for the Notes app: push, then pull, in one run
over one connection (one icloud-sessiond `Session()` call, one TLS
connection, one lock). Each half is exactly `push` and `pull`; the pull runs
unless the push found the sign-in gone or iCloud out of reach. With
`--json` stdout is `{"push": HALF, "pull": HALF|null, "vault_info": …}`
even when a half failed, HALF being `{ok, exit_code, result, lines}` (the
command's own JSON and its human report) or `{ok, exit_code, error}`; it
exits with the worse half's code, 2 (sign-in) above all. No network is the
error `offline` (nothing reachable at all: icloud-session says so at once)
or `network`, exit 1.

clone, pull, push, sync and restore take the vault's lock, the one the Notes app
holds while it is open (`--wait SECS` to wait for it; busy is the error
`vault_busy`, exit 1); status, history, diff and push --dry-run only read and
take none.

History lives in `.icloud-md/history/<record>/` (one JSON snapshot of a
note's or table's CloudKit text per version that pull or push saw) and
`.icloud-md/history/<note>/epochs/` (one entry per run that changed the
note, naming the snapshot current for each of its records). It is kept
bounded: each record keeps its newest 20 snapshots, plus the newest one of
each day for the last 30 days, and each note its epochs by the same rule;
a snapshot a kept epoch names is never dropped. Pruning happens when a
version is recorded, and touches only that note's directories. Recording
reads only the latest snapshot's file. status and push --dry-run record no
history (icloud-md 0.6.2 records one during planning). `vault-info` prints what the app reads from the vault's state
(docs/PORT_PLAN.md §1). In the vault Notes syncs (`~/Documents/icloud-notes`),
use `icloud-notes` (see [docs/AGENTS.md](../docs/AGENTS.md)).

The app finds the engine at `$ICLOUD_NOTES_SYNC_BIN` when that is set (the
Qt tests point it at a stub; point it at `target/debug/icloud-notes-sync`
to run the app against a development build), else at
`/usr/lib/icloud-notes/icloud-notes-sync`, else as `icloud-notes-sync` on
PATH.

Vaults are icloud-md vaults (`.icloud-md/state.json`, layout version 3) and
stay readable by icloud-md.

## Development

```bash
cargo build -p icloud-notes-sync
cargo clippy -p icloud-notes-sync --all-targets
cargo test -p icloud-notes-sync
```

The recorded CLI scenarios (cassettes, expected results, and how to
re-record them) are described in
[tests/differential/README.md](tests/differential/README.md). Every test with
recorded expectations (scenarios, `tests/golden/`, the real-fixture goldens)
rewrites them from the current code with

```bash
ICLOUD_NOTES_SYNC_REGEN=1 cargo test -p icloud-notes-sync
```

after which `git diff` shows what changed - review it before committing.

A live write test against a real iCloud account, confined to one test
folder and checked against a fresh clone, lives in
[tests/live/](tests/live/README.md). It only runs with
`ICLOUD_NOTES_SYNC_LIVE=1`.

## License

MIT, see [LICENSE](../LICENSE). Derived from icloud-md (MIT, Adam Coddington)
and node-diff3 (MIT); see [NOTICE](../NOTICE).
