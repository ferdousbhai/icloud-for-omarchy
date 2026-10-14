# icloud-notes-sync

The sync engine inside the [icloud-notes](../notes/README.md) package: your
iCloud Notes as a folder of Markdown files, synced both ways, in Rust, with
[icloud-session](../session) for the Apple sign-in. It was originally
derived from [icloud-md](https://github.com/coddingtonbear/icloud-md) 0.6.2
by Adam Coddington (MIT; see [NOTICE](../NOTICE)) and has gone its own way
since.

It implements clone, pull, status, push (with `--dry-run`), sync, history,
diff and restore, over the note codec (protobuf, the Notes document model,
tables, attachments), the Markdown renderer and parser, diff3 merging and
the CloudKit client. The tests are unit suites (many first derived from
icloud-md's), golden corpora, recorded CloudKit sessions replayed end to
end, and recorded CLI scenarios that require the same exit codes, output,
requests and vault files as when they were recorded. A live write test runs
against a real account on request only (see below). How it behaves where
that isn't obvious, and why, is in [docs/DESIGN.md](docs/DESIGN.md).

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
$ icloud-notes-sync --quiet status
$ icloud-notes-sync push
$ icloud-notes-sync sync
$ icloud-notes-sync history <file>
$ icloud-notes-sync diff <file> <id>|<from>..<to>
$ icloud-notes-sync restore <file>
$ icloud-notes-sync --quiet vault-info
```

Exit codes, the table the other iCloud tools share
([docs/CLI.md](../docs/CLI.md)); the JSON `code` tells errors with the same
exit apart:

| exit | meaning | codes |
|---|---|---|
| 0 | ok (`status`, `push --dry-run` and `diff` report their changes in the result) | |
| 1 | error | `internal`, `io`, `vault_busy`, `vault_lock`, `vault_needs_update`, `vault_from_newer_tool`, `unsupported_vault_layout`, `corrupt_state_file`, `version_content_unavailable` |
| 2 | usage | `usage`, `ambiguous_tracked_file`, `already_cloned_directory` |
| 3 | not signed in, or the vault isn't set up for this account | `sign_in_required` (sign in with icloud-session), `not_cloned_directory`, `unbound_account`, `account_mismatch`, `requested_account_mismatch`, `notes_unavailable` |
| 4 | not found | `untracked_file`, `unknown_version_snapshot`, `cloudkit_zone_fetch_failed` for a `ZONE_NOT_FOUND` zone |
| 5 | a service unavailable | `offline`, `network`, `session_service` (icloud-sessiond unreachable), `keyring_unavailable`, `cloudkit_request_failed`, `cloudkit_zone_fetch_failed` |

Output follows the contract every iCloud tool shares
([icloud_session::output](../session/src/output.rs)): text on a terminal;
when piped, or with `--json`, the envelope `{"ok":true,"data":…,"summary":"…"}`;
`--quiet` prints only `data` (the command's result: what the Notes app
reads); `--styled` forces text; `--count` prints how many entries
(`status`, `push`, `pull` changes, `history`, `vault-info` notes) and
`--ids-only` their files (`status`, `push`, `restore`) or snapshot ids
(`history`). The flags combine: ids-only > count > quiet > json > styled. In
those machine modes stdout is exactly one JSON document, and status lines
and progress go to stderr. A failure is
`{"ok":false,"error":{"code":"sign_in_required","message":…,"hint":…}}`
on stdout in a machine mode, pretty-printed (`code` is the error's class in
snake case; `hint`, when present, says how to fix it), and `error: …` /
`hint: …` on stderr in text mode.

`sync` is for the Notes app: push, then pull, in one run
over one connection (one icloud-sessiond `Session()` call, one TLS
connection, one lock). Each half is exactly `push` and `pull`; the pull runs
unless the push found the sign-in gone or iCloud out of reach. Its data
is `{"push": HALF, "pull": HALF|null, "vault_info": …}` even when a half
failed, HALF being `{ok, exit_code, result, lines}` (the command's own data
and its human report) or `{ok, exit_code, error}` (`{code, message,
hint?}`); a failed half is reported only there, with no separate error
object. It exits 3 if either half exited 3, else with the higher of the two halves'
codes. No network is the error `offline` (nothing reachable at all:
icloud-session says so at once) or `network`, exit 5.

clone, pull, push, sync and restore take the vault's lock, the one the Notes app
holds while it is open (`--wait SECS` to wait for it; busy is the error
`vault_busy`, exit 1); status, history, diff and push --dry-run only read and
take none.

A vault keeps the engine's state in `.icloud-notes/` (vault layout 4):
`state.json`, `base/` (each note's last-synced body, the merge base),
`history/` and the app's `conflict-backups/`. A vault from an older engine
(or icloud-md) keeps them in `.icloud-md/` (layout 3). Read-only commands
read it there; the first command that takes the lock moves it to
`.icloud-notes/`, after copying `.icloud-md/` to `.icloud-md.bak-<UTC time>`
(kept: delete it once you are happy), and leaves a tombstone
`.icloud-md/state.json` that makes an older engine refuse the vault as
written by a newer version. Note files are not touched. See
docs/DESIGN.md §1.

History lives in `.icloud-notes/history/<record>/` (one JSON snapshot of a
note's or table's CloudKit text per version that pull or push saw) and
`.icloud-notes/history/<note>/epochs/` (one entry per run that changed the
note, naming the snapshot current for each of its records). It is kept
bounded: each record keeps its newest 20 snapshots, plus the newest one of
each day for the last 30 days, and each note its epochs by the same rule;
a snapshot a kept epoch names is never dropped. Pruning happens when a
version is recorded, and touches only that note's directories. Recording
reads only the latest snapshot's file. status and push --dry-run record no
history. `vault-info` prints what the app reads from the vault's state,
`stateDir` included. In the vault Notes syncs (`~/Documents/Notes`),
use `icloud-notes` (see [docs/AGENTS.md](../docs/AGENTS.md)).

The app finds the engine at `$ICLOUD_NOTES_SYNC_BIN` when that is set (the
Qt tests point it at a stub; point it at `target/debug/icloud-notes-sync`
to run the app against a development build), else at
`/usr/lib/icloud-notes/icloud-notes-sync`, else as `icloud-notes-sync` on
PATH.

state.json's `sharedDatabase` records where the shared-database listing
left off, so a pull walks only the shared zones that changed (and lists them
all from scratch at least daily, or when the key is missing). A pull or
clone looks up the attachments of all its notes in one `records/lookup`
walk per zone and record type, and downloads up to four attachment files at
once. Both are in docs/DESIGN.md §1. In a machine mode (`--json`,
`--quiet`, piped, ...), progress goes to stderr as
`icloud-notes:progress:...` lines.

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

MIT, see [LICENSE](../LICENSE). Originally derived from icloud-md (MIT, Adam
Coddington) and node-diff3 (MIT); see [NOTICE](../NOTICE).
