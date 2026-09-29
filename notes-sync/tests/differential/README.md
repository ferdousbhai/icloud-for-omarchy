# Differential harness

Runs the unmodified icloud-md 0.6.2 clone and icloud-notes-sync against the
same recorded CloudKit answers (a *cassette*), then compares what each did:
the vault on disk (`diff -r`), `--json` output, exit codes, and every request
sent to CloudKit.

- Node side: `driver.mts` runs icloud-md's own `src/cli.ts` in-process with
  `HOME` pointing at a temp dir holding a fake
  `~/.config/icloud-md/accounts/<dsid>/{session.local.json,meta.json}`, and
  `globalThis.fetch` replaced by a stub that answers `/validate` and
  ckdatabasews (and asset) requests from the cassette and logs each request.
  No browser, no network.
- Rust side: `cloudkit::transport::ReplayTransport` reads the same cassette
  and writes the same request log (`ICLOUD_NOTES_SYNC_CASSETTE`,
  `ICLOUD_NOTES_SYNC_REQUEST_LOG`); stubbed until workstream A fills it in.

## Requirements

The icloud-md clone with its `node_modules` installed (it ships `tsx`). The
default location is `../../coddingtonbear/icloud-md` relative to this repo
(`/home/dous/github.com/coddingtonbear/icloud-md`); override with
`ICLOUD_MD=/path/to/icloud-md`. Node ≥ 20.

## Running icloud-md

```bash
ICLOUD_MD=${ICLOUD_MD:-../../coddingtonbear/icloud-md}
OUT=tests/differential/out/tiny        # git-ignored
$ICLOUD_MD/node_modules/.bin/tsx tests/differential/driver.mts \
  --cassette tests/differential/cassettes/tiny-clone.json \
  --requests $OUT/requests.json --home $OUT/home \
  --now 1790000000000 --deterministic \
  -- --json clone --account 10000000001 --non-interactive $OUT/vault \
  > $OUT/stdout.json 2> $OUT/stderr.txt
echo "exit $?"
```

Driver options (icloud-md's own arguments go after `--`):

| option | meaning |
|---|---|
| `--cassette FILE` | answers to serve (required) |
| `--requests FILE` | write the request log here |
| `--home DIR` | fake HOME (default: a fresh temp dir); icloud-md's debug log lands in `DIR/.config/icloud-md/` |
| `--cwd DIR` | chdir first (for vault-root discovery: `pull` with no directory) |
| `--now MS` | freeze the clock: `Date.now()` and `new Date()` return this ms epoch |
| `--deterministic` | replace randomness (below) |

The exit code is icloud-md's; stdout and stderr are icloud-md's too (plus
`[driver] no cassette interaction for ...` on stderr for an unmatched
request, which gets HTTP 599).

The tiny cassette clones to `Notes/Test Note.md` (front matter
`apple-note-id`, then `# Test Note` and one body line), its base copy, and
`state.json`; the log shows `/validate`, private `changes/zone` and shared
`changes/database`.

### Determinism

`--deterministic` makes every random value icloud-md draws reproducible, so
push request bodies, epoch ids and snapshot ids can be compared byte for byte.
The Rust side has to draw from the same streams in the same order when
`ICLOUD_NOTES_SYNC_DETERMINISTIC=1` (and freeze its clock under
`ICLOUD_NOTES_SYNC_NOW=<ms>`):

- UUIDs (`randomUUID` from `node:crypto` and Web Crypto's
  `crypto.randomUUID`, one shared counter): the n-th call (1-based) returns
  `00000000-0000-4000-8000-<n as 12 lowercase hex digits>`. `/validate`'s
  `requestId` draw is handed back, since the Rust side never calls
  `/validate`.
- `randomBytes(k)`: the m-th call (1-based) returns the bytes
  `(m + j) & 0xff` for `j = 0..k-1`.

File mtimes that icloud-md sets from note dates (`noteTimestamps.ts`) are
deterministic already; mtimes of files it merely writes are not, so compare
with `diff -r` (contents) and check note mtimes explicitly.

Normalize before comparing: `state.json`'s `generator`
(`icloud-md 0.6.2` vs `icloud-notes-sync X.Y.Z`) and, in request logs,
`service: "setup"` entries (Node only).

## Cassette format (version 1)

```json
{
  "version": 1,
  "account": { "dsid": "10000000001", "appleId": "harness@example.com" },
  "ckdatabasewsUrl": "https://p00-ckdatabasews.icloud.com:443",
  "validate": { "dsInfo": { "...": "..." }, "webservices": { "...": "..." } },
  "interactions": [
    {
      "note": "free text",
      "request": {
        "method": "POST",
        "path": "/database/1/com.apple.notes/production/private/changes/zone",
        "body": { "zones": [ "..." ] }
      },
      "response": { "status": 200, "body": { "zones": [ "..." ] } }
    },
    {
      "request": { "method": "GET", "url": "https://cvws.icloud-content.com/B/abc" },
      "response": { "status": 200, "bodyBase64": "..." },
      "repeat": true
    }
  ]
}
```

| key | meaning |
|---|---|
| `account` | the account icloud-md's fake session is for; `clone --account` takes this dsid |
| `ckdatabasewsUrl` | optional; default `https://p00-ckdatabasews.icloud.com:443` |
| `validate` | optional raw `/validate` answer; default is `dsInfo` from `account` plus `webservices.ckdatabasews.url`. Only the Node side calls `/validate` |
| `interactions[].request.method` | HTTP method |
| `request.path` | ckdatabasews path, without the query string |
| `request.url` | absolute URL for any other host (signed asset downloads); its query is ignored unless it has one |
| `request.body` | optional; when present the request body must be JSON-equal to it |
| `response.status` | default 200 |
| `response.body` / `bodyBase64` | JSON answer, or raw bytes (assets) |
| `repeat` | reusable; otherwise each interaction answers once |

Matching: each request takes the first unused interaction with the same
method and path (or URL) whose `body`, if given, matches. So a paged
`changes/zone` is a sequence of interactions for the same path, in order; use
`body` to pin one to a particular `syncToken` where order alone is
ambiguous.

The Rust types are `cloudkit::transport::{Cassette, Interaction,
CassetteRequest, CassetteResponse}`.

## Request log format

```json
{
  "requests": [
    {
      "method": "POST",
      "service": "ckdatabasews",
      "path": "/database/1/com.apple.notes/production/private/changes/zone",
      "query": { "ckjsBuildVersion": "2310ProjectDev27", "ckjsVersion": "2.6.4" },
      "body": { "zones": [ "..." ] },
      "matched": 0
    }
  ]
}
```

`service` is `setup` (`/validate`; answered from `validate`, `matched`
null), `ckdatabasews`, or `other` (then `path` is the absolute URL without
its query). `query` drops the per-session parameters (`clientId`,
`clientBuildNumber`, `clientMasteringNumber`, `dsid`, `requestId`) and sorts
the rest. `body` is the parsed JSON body (or the raw string), absent when
there was none. `matched` is the index of the interaction that answered, or
null when none did. Rust: `cloudkit::transport::{RequestLog, LoggedRequest}`.

## Scenarios

`scenarios.json` lists the comparisons; `expected/<name>/` holds what
icloud-md did for each, regenerated with

```bash
tests/differential/regen.py [scenario ...]
```

which prepares the vault, runs `run-node.sh` on the scenario's cassette and
stores `exit`, `stdout.json` (the out-dir path replaced by `@OUT@`),
`requests.json`, `vault/` and `mtimes.json` (the mtimes of the vault's files
that are deterministic: set from note dates, or by the setup). Commit the
result.

| key | meaning |
|---|---|
| `name` | `expected/<name>/` |
| `cassette` | file in `cassettes/` |
| `args` | icloud-md / icloud-notes-sync arguments; `@VAULT@` = the vault, `@OUT@` = the run's temp dir |
| `vaultFrom` | start from a copy of `expected/<scenario>/vault` (an earlier scenario) instead of nothing |
| `edits` | applied in order: `{file, write}`, `{file, append}`, `{file, replace: [old, new]}` (first occurrence), `{file, delete: true}`, `{file, json: {key: value or null}}` (set in place / remove, rewritten 2-space + newline) |
| `cwd` | working directory (default `@OUT@`) |
| `now` | frozen clock, ms (default in `defaults`) |
| `setupMtimeMs` | after the edits, every file outside `.icloud-md/` gets this mtime (push reads file mtimes into request bodies) |
| `compare` | subset of `exit`, `stdout`, `requests`, `vault`, `mtimes` (default all) |

The Rust side is `tests/cli_differential.rs`: it prepares each scenario the
same way, runs the `icloud-notes-sync` binary with
`ICLOUD_NOTES_SYNC_CASSETTE`, `ICLOUD_NOTES_SYNC_REQUEST_LOG`,
`ICLOUD_NOTES_SYNC_NOW` and `ICLOUD_NOTES_SYNC_DETERMINISTIC=1`, and compares
exit code, stdout, the request log (icloud-md's minus `setup` entries), the
vault tree (`generator` normalized) and `mtimes.json`. The full run is
`#[ignore]`d until the codec, Markdown and CloudKit workstreams land:

```bash
cargo test --test cli_differential -- --ignored
ICLOUD_NOTES_SYNC_DIFF_ONLY=tiny-clone,tiny-push cargo test --test cli_differential -- --ignored
```

## Cassettes

| file | what |
|---|---|
| `cassettes/tiny-clone.json` | private zone with the default folder and one plain note (`REAL_PLAIN_NOTE`), no shared zones |
| `cassettes/tiny-lookup.json` | `records/lookup` answering that note unchanged |
| `cassettes/tiny-lookup-edited.json` | the same lookup after the remote edit below |
| `cassettes/tiny-pull-noop.json` | a pull with nothing changed |
| `cassettes/tiny-pull-update.json` | a pull delivering the note edited on another device (a line appended via icloud-md's own `applyTextEdit`; tag `26a`) |
| `cassettes/tiny-push.json` | lookup + the `records/modify` answer for an update (tag `26b`) |
| `cassettes/tiny-push-create.json` | the `records/modify` answer for a create |
| `cassettes/tiny-push-delete.json` | lookup + the answer for the trash move |

Randomness in icloud-md that the Rust side must mirror through
`vault::rt`: `recordVersion`/`recordEpoch` ids, `planFolderCreates` record
names, push's create record name and replica id, and formatReconcile's
per-paragraph `uuidBytes()` (drawn for every planned paragraph, used or
not - workstream B has to draw the same way).

Planned (PORT_PLAN §4.2): cassettes recorded from read-only live sessions,
hand-mutated copies for push, and runs of pull/status/push --dry-run on a
`cp -a` copy of the real vault.

## Fixtures

`export-fixtures.mts` regenerates `tests/fixtures/real/` (see
`tests/fixtures/README.md`) with the same icloud-md import mechanism.
