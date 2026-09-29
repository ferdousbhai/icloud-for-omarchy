# icloud-notes-sync

Your iCloud Notes as a folder of Markdown files, synced both ways from the
command line. A Rust port of [icloud-md](https://github.com/coddingtonbear/icloud-md)
0.6.2 by Adam Coddington, using [icloud-session](../session) for the
Apple sign-in instead of a browser of its own.

Work in progress: the crate currently holds the port's interfaces with
`todo!()` bodies. The plan is [docs/PORT_PLAN.md](docs/PORT_PLAN.md).

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
