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

## Cassettes

| file | what |
|---|---|
| `cassettes/tiny-clone.json` | private zone with the default folder and one plain note (`REAL_PLAIN_NOTE`), no shared zones |
| `cassettes/tiny-lookup.json` | `records/lookup` answering that note unchanged (for status / push --dry-run on the tiny clone) |

`expected/` holds icloud-md's results for them, as the Rust side's first
targets: `tiny-clone/` (`--json clone --account 10000000001
--non-interactive <vault>` with `--now 1790000000000`: stdout, exit code,
request log and the resulting vault) and `tiny-push-dry-run/` (`--json push
--dry-run` on that vault after appending a line to `Notes/Test Note.md`:
exit 3, one ready update).

`run-node.sh CASSETTE OUTDIR [driver options] -- ARGS` wraps the driver
(always `--deterministic`, `HOME=OUTDIR/home`) and writes
`OUTDIR/{stdout,stderr,exit,requests.json}`; `@OUT@` in ARGS expands to
OUTDIR:

```bash
tests/differential/run-node.sh tests/differential/cassettes/tiny-clone.json \
  tests/differential/out/tiny --now 1790000000000 \
  -- --json clone --account 10000000001 --non-interactive @OUT@/vault
```

Planned (PORT_PLAN §4.2): cassettes recorded from read-only live sessions,
hand-mutated copies for push, and runs of pull/status/push --dry-run on a
`cp -a` copy of the real vault.

## Fixtures

`export-fixtures.mts` regenerates `tests/fixtures/real/` (see
`tests/fixtures/README.md`) with the same icloud-md import mechanism.
