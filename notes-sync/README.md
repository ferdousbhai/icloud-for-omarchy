# icloud-notes-sync

Your iCloud Notes as a folder of Markdown files, synced both ways from the
command line. A Rust port of [icloud-md](https://github.com/coddingtonbear/icloud-md)
0.6.2 by Adam Coddington, using [icloud-session](../session) for the
Apple sign-in instead of a browser of its own.

Every command below is implemented: clone, pull, status, push (with
`--dry-run` and every refusal), history, diff and restore, over the note
codec (protobuf, the Notes document model, tables, attachments), the
Markdown renderer and parser, diff3 merging and the CloudKit client. The
tests are icloud-md's own test suites ported to Rust, golden outputs from
icloud-md's code, recorded CloudKit sessions replayed end to end, and a
differential suite (`cargo test -p icloud-notes-sync -- --ignored`) that runs
icloud-md itself on the same inputs and requires the same exit codes,
output, requests and vault files. A live write test runs against a real
account on request only (see below). How the port was planned and where it
deliberately differs from icloud-md 0.6.2 is in
[docs/PORT_PLAN.md](docs/PORT_PLAN.md).

```console
$ icloud-notes-sync clone --account <dsid> ~/Notes
$ icloud-notes-sync pull
$ icloud-notes-sync --json status
$ icloud-notes-sync push
$ icloud-notes-sync history <file>
$ icloud-notes-sync diff <file> <id>|<from>..<to>
$ icloud-notes-sync restore <file>
```

Exit codes: 0 ok, 1 error, 2 usage, 3 `status`/`push --dry-run` has
entries or `diff` found differences, 4 iCloud sign-in required, 70 internal
error.

Vaults are icloud-md vaults (`.icloud-md/state.json`, layout version 3) and
stay readable by icloud-md.

## Development

```bash
cargo build -p icloud-notes-sync
cargo clippy -p icloud-notes-sync --all-targets
cargo test -p icloud-notes-sync
cargo test -p icloud-notes-sync -- --ignored   # needs the icloud-md clone and node
```

The differential harness that runs icloud-md itself against the same
recorded CloudKit answers is described in
[tests/differential/README.md](tests/differential/README.md).

A live write test against a real iCloud account, confined to one test
folder and checked against icloud-md, lives in
[tests/live/](tests/live/README.md). It only runs with
`ICLOUD_NOTES_SYNC_LIVE=1`.

## License

MIT, see [LICENSE](../LICENSE). Derived from icloud-md (MIT, Adam Coddington)
and node-diff3 (MIT); see [NOTICE](../NOTICE).
