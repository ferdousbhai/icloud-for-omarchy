# Recorded CLI scenarios

End-to-end regression tests for the `icloud-notes-sync` binary. Each
scenario runs the binary against recorded CloudKit answers (a *cassette*),
with the clock and randomness pinned, and compares what it did with what an
earlier run recorded in `expected/<scenario>/`: the exit code, the `--json`
output, every request sent to CloudKit, the vault on disk and the file
mtimes that don't depend on when the test ran.

The expectations are recorded from this crate, so a deliberate behaviour
change is re-recorded and reviewed like any other code change; no Node or
icloud-md is needed. (History: the first recordings came from icloud-md
0.6.2 itself, which the engine was derived from, hence the directory's name,
`differential`; see docs/DESIGN.md.)

The test is `tests/cli_differential.rs`:

```bash
cargo test -p icloud-notes-sync --test cli_differential
ICLOUD_NOTES_SYNC_DIFF_ONLY=tiny-clone,tiny-push cargo test -p icloud-notes-sync --test cli_differential
```

A few scenarios also have named checks on what their recording must show
(`dup_clone_*`, `bodyless_pull_*`, `bodyless_clone_*`, `attach_*`,
`shared_pull_*`, and the vault-layout ones below). These catch a
re-recording that quietly accepts a regression.

Vault layouts (all on `tiny-lookup.json`, from the `tiny-clone` vault turned
back into an older layout with a `renameTo` edit):

- `tiny-migrate-v2`: push migrates a layout 2 vault through 3 to 4;
- `tiny-status-v2`: status refuses a layout 2 vault (`vault_needs_update`,
  exit 1) and migrates nothing;
- `tiny-status-v3`: status reads a layout 3 vault in place, no migration;
- `tiny-migrate-v3`: restore moves a layout 3 vault's state to
  `.icloud-notes/`, leaving a backup (`.icloud-md.bak-<time>`) and a
  tombstone `.icloud-md/state.json`.

## Re-recording

After an intended behaviour change, or after adding a scenario:

```bash
ICLOUD_NOTES_SYNC_REGEN=1 cargo test -p icloud-notes-sync --test cli_differential
git diff tests/differential/expected     # review every change before committing
cargo test -p icloud-notes-sync --test cli_differential
```

`ICLOUD_NOTES_SYNC_REGEN=1` runs every scenario in manifest order (so a
`vaultFrom` scenario starts from the vault just recorded for the earlier
one) and rewrites `expected/<name>/` from the run:

- `exit`: the exit code
- `stdout.json`: the `--json` output, with the run's temp dir replaced by
  `@OUT@`
- `requests.json`: the request log (`{"requests": []}` when nothing was
  sent)
- `vault/`: the vault tree, with state.json's `generator` replaced by
  `<generator>` so that a version bump changes nothing
- `mtimes.json`: the mtimes of the vault files that were set from note dates
  or by the setup, not by the write itself

`ICLOUD_NOTES_SYNC_DIFF_ONLY` limits a re-recording too. Leave out a
scenario other scenarios start from only if its recording is unchanged.
The same switch re-records the golden corpora and the fixture goldens:
`ICLOUD_NOTES_SYNC_REGEN=1 cargo test -p icloud-notes-sync` re-records all of
them (notes-sync/README.md).

## Scenarios

`scenarios.json` lists them, in order.

| key | meaning |
|---|---|
| `name` | `expected/<name>/` |
| `cassette` | file in `cassettes/` |
| `args` | icloud-notes-sync arguments; `@VAULT@` = the vault, `@OUT@` = the run's temp dir |
| `vaultFrom` | start from a copy of `expected/<scenario>/vault` (an earlier scenario) instead of nothing |
| `edits` | applied in order: `{file, write}`, `{file, append}`, `{file, replace: [old, new]}` (first occurrence), `{file, delete: true}`, `{file, renameTo: path}` (renames a file or directory in the vault, e.g. `.icloud-notes` → `.icloud-md` to turn a recorded layout 4 vault back into a layout 3 one), `{file, json: {key: value or null}}` (set in place / remove, rewritten 2-space + newline) |
| `cwd` | working directory (default `@OUT@`) |
| `now` | frozen clock, ms (default in `defaults`) |
| `setupMtimeMs` | after the edits, every file outside the state directories (`.icloud-notes/`, and a layout 3 vault's `.icloud-md/` and `.icloud-md.bak-*/`) gets this mtime (push reads file mtimes into request bodies) |
| `compare` | subset of `exit`, `stdout`, `requests`, `vault`, `mtimes` to compare (default all; everything is recorded regardless) |

The binary runs with `HOME` and `XDG_RUNTIME_DIR` in the temp dir,
`ICLOUD_NOTES_SYNC_CASSETTE` (answer from the cassette through
`cloudkit::transport::ReplayTransport`; nothing reaches the network),
`ICLOUD_NOTES_SYNC_REQUEST_LOG`, `ICLOUD_NOTES_SYNC_NOW=<now>` and
`ICLOUD_NOTES_SYNC_DETERMINISTIC=1`.

### Determinism

Under `ICLOUD_NOTES_SYNC_DETERMINISTIC=1` every random value is reproducible
(`vault::rt`), so push request bodies, epoch ids and snapshot ids are the
same on every run:

- UUIDs: the n-th call (1-based) returns
  `00000000-0000-4000-8000-<n as 12 lowercase hex digits>`.
- `random_bytes(k)`: the m-th call (1-based) returns the bytes
  `(m + j) & 0xff` for `j = 0..k-1`.

`ReplayTransport` offers no shared downloader, so attachment downloads run
one at a time in the order the notes queue them (live, up to four overlap)
and the request log is the same on every run.

Randomness drawn through `vault::rt`: `recordVersion`/`recordEpoch` ids,
folder-create record names, push's create record name and replica id, and
reconcile's per-paragraph uuid bytes (drawn for every planned paragraph,
used or not). Changing the order or number of draws changes the recorded
request bodies and history files.

## Cassette format (version 1)

```json
{
  "version": 1,
  "account": { "dsid": "10000000001", "appleId": "harness@example.com" },
  "ckdatabasewsUrl": "https://p00-ckdatabasews.icloud.com:443",
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
| `account` | the account the session is for; `clone --account` takes this dsid or Apple ID |
| `ckdatabasewsUrl` | optional; default `https://p00-ckdatabasews.icloud.com:443` |
| `validate` | optional; a raw `/validate` answer, kept from the icloud-md recordings and unused by the binary |
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
ambiguous. An unmatched request gets HTTP 599.

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

`service` is `ckdatabasews` or `other` (then `path` is the absolute URL
without its query). `query` drops the per-session parameters (`clientId`,
`clientBuildNumber`, `clientMasteringNumber`, `dsid`, `requestId`) and sorts
the rest. `body` is the parsed JSON body (or the raw string), absent when
there was none. `matched` is the index of the interaction that answered, or
null when none did. Rust: `cloudkit::transport::{RequestLog, LoggedRequest}`.

## Cassettes

| file | what |
|---|---|
| `tiny-clone.json` | private zone with the default folder and one plain note (`REAL_PLAIN_NOTE`), no shared zones |
| `tiny-lookup.json` | `records/lookup` answering that note unchanged |
| `tiny-pull-noop.json` | a pull with nothing changed |
| `tiny-pull-update.json` | a pull delivering the note edited on another device (a line appended; tag `26a`) |
| `tiny-push.json` | lookup + the `records/modify` answer for an update (tag `26b`) |
| `tiny-push-create.json` | the `records/modify` answer for a create |
| `tiny-push-delete.json` | lookup + the answer for the trash move |
| `tiny-sync.json` | `tiny-push.json` then `tiny-pull-noop.json`: scenario `tiny-sync` pushes and pulls in one run, over one connection (one request log with all four requests) |
| `dup-clone.json` | `tiny-clone` split over two `changes/zone` pages, the note repeated on the second (what live clones hit about 1 in 9 times); scenario `dup-clone` writes the note once: the same vault and output as `tiny-clone` |
| `bodyless-pull.json` | a pull whose private listing adds a note (`Fresh`, tag `27a`) without its `TextDataEncrypted`, and a private `records/lookup` answering it with its text; scenario `bodyless-pull` looks it up and adds it |
| `bodyless-pull-unfilled.json` | the same, with the lookup also answering without the text; scenario `bodyless-pull-unfilled` skips the note, keeps the previous private sync token and warns |
| `bodyless-clone.json` | `tiny-clone` with `Fresh` also listed without its text, and the private `records/lookup` answering it with its text; scenario `bodyless-clone` looks it up and writes it |
| `bodyless-clone-unfilled.json` | the same, with the lookup also answering without the text; scenario `bodyless-clone-unfilled` skips the note, saves no private sync token and warns |
| `asset-clone.json` | notes whose text is only a `TextDataAsset` (private, and shared via the `records/lookup` backfill); downloaded, inlined, and marked read-only |
| `asset-clone-download-failed.json` | the same with the asset download failing: the clone fails |
| `asset-lookup.json` | lookup for status/push of the read-only asset note (both refuse it) |
| `asset-pull-update.json` | a pull delivering a changed asset note |
| `attach-clone.json` | `tiny-clone` plus two notes with one file attachment each (a recording, a photo); one repeatable `records/lookup` answering all their Attachment and Media records, and the two asset GETs |
| `attach-pull.json` | a pull delivering those two notes, with the same lookup and assets; scenarios `attach-clone` and `attach-pull` look the attachments up in one lookup per record type, not two per note |
| `shared-pull-unchanged.json` | a pull of the `asset-clone` vault: the shared `changes/database` resumed from its stored token (pinned) reports no zone changed, so no shared zone is walked |
| `shared-pull-revoked.json` | the same, reporting the shared zone `deleted`: its note is untracked |
| `shared-full-listing.json` | the resumed listing rejected (HTTP 400), then a listing from scratch and an unchanged shared zone; scenarios `shared-pull-token-rejected`, `shared-pull-old-state` (no `sharedDatabase` in state) and `shared-pull-stale-cursor` (a day later) |
