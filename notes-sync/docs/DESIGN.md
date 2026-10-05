# notes-sync design notes

notes-sync (`icloud-notes-sync`) is the sync engine behind icloud-notes:
clone, pull, push, status, history, diff, restore, sync and vault-info over
a vault of Markdown files. These notes record how it behaves where the
behaviour isn't obvious, and why.

## History

The engine began as a Rust port of
[icloud-md](https://github.com/coddingtonbear/icloud-md) 0.6.2 (MIT, Adam
Coddington), held for a while to the same exit codes, output, CloudKit
requests and vault files, checked against icloud-md's own results over the
same recorded inputs. Apple calls went through icloud-session (`session/`)
instead of icloud-md's sign-in from the start. The tests no longer run
icloud-md: the recorded scenarios in `tests/differential/` (see its README,
run from `tests/cli_differential.rs`), the golden corpora in `tests/golden/`
and the real-fixture goldens are regression tests recorded from this crate
(re-recorded with `ICLOUD_NOTES_SYNC_REGEN=1`). The engine now goes its own
way; the sections below say where it differs from icloud-md 0.6.2 when that
helps explain a choice. The code derived from icloud-md is credited in
[NOTICE](../../NOTICE).

## 1. Behaviour decisions

### The vault and its state directory (layout 4)

A vault keeps the engine's state in `.icloud-notes/`: `state.json`,
`base/<recordName>.md` (each note's last-synced body, the merge base),
`history/` (snapshots and epochs, below) and `conflict-backups/` (copies the
Notes app makes before replacing a note whose conflict markers can't be
read). Every path into it is built in `src/vault/state.rs` (`state_dir`,
`state_subdir`, `state_file_path`).

Layout 3 and older (icloud-md's, and this engine's before layout 4) kept the
same things in `.icloud-md/`. The 3 → 4 migration (`src/vault/migrate.rs`)
runs only in commands that hold the vault lock (clone, pull, push, restore,
sync), the first time one opens such a vault:

1. Unless `.icloud-notes/` already exists (an earlier attempt got past this
   step), `.icloud-md/` is copied to `.icloud-md.bak-<UTC time>` (through a
   `.partial` directory renamed into place, so a backup that exists is
   complete; a leftover `.partial` is removed and the copy redone). The
   backup is kept: the user can delete it.
2. `.icloud-notes/` is created, and `base/`, `history/` and
   `conflict-backups/` are renamed into it (same filesystem). Where one
   already exists on both sides, what the new side lacks is moved over.
3. The new state.json (`layoutVersion: 4`) is written into `.icloud-notes/`
   through a temp file and a rename: the commit point.
4. `.icloud-md/state.json` is replaced with a tombstone,
   `{"layoutVersion":4,"movedTo":".icloud-notes"}`, so an older engine
   refuses the vault as written by a newer version instead of treating it as
   not cloned, and an older app (which looked for `.icloud-md`) still sees a
   cloned vault. Every later open under the lock re-checks this, in case a
   run stopped between 3 and 4.

Each step is idempotent, so a run interrupted anywhere finishes on the next
one. Note files are never rewritten (frontmatter keeps `apple-note-id`).

Read-only commands (status, push --dry-run, history, diff, vault-info) never
migrate: a layout 3 vault is read where it is. While the state is still in
`.icloud-md/`, a subdirectory an interrupted migration already moved is
found in `.icloud-notes/`. A layout 2 vault is migrated (2 → 3 stamps each
note file with its `apple-note-id`, then 3 → 4) only by a locked command;
read-only commands refuse it with `vault_needs_update` (exit 1). Fresh clones
write layout 4 directly. Both `.icloud-notes` and `.icloud-md` are reserved
top-level directory names (a Notes folder with that name gets another
directory name).

state.json is plain serde structs written in their field order (2-space
JSON, trailing newline, absent values omitted), stamped with `layoutVersion`
and `generator` (`icloud-notes-sync X.Y.Z`). Reading accepts the keys in any
order and ignores unknown keys inside entries; unknown top-level keys are
kept and written back after the known ones. (icloud-md wrote different key
orders from different code paths, which the engine used to reproduce.)

### The vault lock, and vault-info

The engine takes the lock icloud-notes holds (`src/cmd/lock.rs`): clone,
pull, push, sync and restore take an exclusive flock on
`$XDG_RUNTIME_DIR/icloud-notes-<FNV-1a 64 of the vault's canonical path>.lock`
(beside the vault without a runtime directory), the path icloud-notes'
`NotesBackend::lockPath()` computes too. Non-blocking: another run or a
background sync is waited for (`--wait SECS`, 30 by default), the Notes
window, which holds it while open, not at all; busy is `vault_busy`, exit 1.
status, history, diff, push --dry-run and vault-info only read and take no
lock, and write nothing (no migration either). A caller already holding the
lock (the app) hands its locked descriptor down as `ICLOUD_NOTES_LOCK_FD`;
it is used only if it is open on this vault's lock file and holds the lock.

`vault-info` (JSON: title mode, default folder, tracked notes with their
read-only reasons and base copies, the state directory (`stateDir`), state
and lock files) is what the app reads instead of parsing state.json. The app
keeps its conflict backups in `stateDir`.

### sync, progress, and network errors

`sync` (`src/cmd/sync.rs`) is push then pull in one run, over one connection
(`remote::SharedConnector`), under one lock: what the app runs on every tick.
The pull runs whatever the push did, except after a sign-in required or a
network error. Its `--json` holds both results plus vault-info's answer.
icloud-session's `Offline` and `Network` errors are the errors `offline` and
`network` (exit 1, with a retry hint) rather than `internal` (70).

With `--json`, progress goes to stderr as `icloud-notes:progress:...` lines
(`fetch:N`, `process-start:N`, `process:I/N`, `process-done`). Engines
before layout 4 said `icloud-md:progress:...`; the app accepts both.

### Notes whose text is a separate asset

icloud-md 0.6.2 skips notes whose body lives in `TextDataAsset` (one user
note is ~833 KB). The engine requests `TextDataAsset`, inlines the
downloaded bytes and marks such notes read-only ("is so large that Apple
keeps its text in a separate file, which can't be written back yet"). This
is upstream PR #29 (ferdousbhai, "Fetch the text of notes too large to store
it inline"): `cloudkit::client` (`note_desired_keys`,
`Database::inline_asset_bodies` at the end of every zone walk and after the
shared lookup backfill; a failed download fails the fetch) and
`doc::decode::classify_note_record` (the PR's reason string). Always on:
every scenario records `TextDataAsset` in its `changes/zone` `desiredKeys`.

Tests: tests/cloudkit_asset_body.rs; the `asset-*` scenarios cover clone, a
failed download, status, push and pull end to end.

### Records listed twice by CloudKit

Live testing (2026-09-29, a ~118-note account) found about 1 clone in 9
writing one note twice: a second byte-identical file under a uniquified
name (`X 2.md`), only one of the two tracked in `state.json`, so the next
`push` would create the other as a duplicate note. icloud-md 0.6.2 does the
same. Cause: a `changes/zone` walk occasionally lists the same record on two
pages (the run logs show the fetch total one higher than the clones before
and after it with no note created in between, and `written` one higher);
concatenating pages and materializing every occurrence writes the note
twice (and `state.json` keeps only the last one, so the first file is
untracked). The engine handles it in two places:

- `cloudkit::client::dedupe_zone_records`, applied at the end of every zone
  walk (`walk_zone`, so private and each shared zone, before the
  `records/lookup` backfill and asset inlining):
  one record per recordName within a zone. The winner is the occurrence with
  the greater `modified.timestamp` when both have one and they differ, else
  the later occurrence in listing order (a later page is a later server read,
  never staler; this also lets a later tombstone replace a live copy).
  `recordChangeTag`s are opaque and never compared. The winner keeps the
  position of the first occurrence. The same recordName in a different zone
  (private vs shared) is a different record and is kept.
  `fetch_shared_zone_ids` likewise keeps a zone listed twice once. `on_page`
  progress still counts raw page sizes. Pull uses the same fetches, so it is
  covered too.
- `push` (and `status`) refuse to create an untracked file whose
  `apple-note-id` names a note this clone tracks while that note's own file is
  still present (`Refusal::CreateDuplicatesTrackedNote`: "carries the
  "apple-note-id" of <tracked file>, a note this clone already tracks, so
  pushing it would create a duplicate of that note - delete this file if it
  is a leftover copy, or remove its "apple-note-id" line to push it as a new
  note"). Moves (the tracked file gone, one claimant) and ambiguous claims
  are unchanged.

Tests: tests/cloudkit_dedupe.rs (same page, across pages, timestamps vs page
order, differing tags, tombstone, private vs shared plus the lookup
backfill, a shared zone listed twice), tests/cmd_push.rs
(`copy_with_original_in_place_is_refused_as_a_duplicate`,
`byte_identical_twin_from_a_double_clone_is_refused`), tests/cmd_refusals.rs,
and the scenario `dup-clone` (`dup_clone_writes_a_repeated_record_once`: both
pages fetched, and the same vault/stdout as `tiny-clone`).

### Local deletes, renames and new files win over a pull

Deleting a note's file is a delete, as deleting the note in Notes is. In
icloud-md 0.6.2 it is only when nothing changed remotely since the last
pull: push refuses with "changed remotely since the last pull - run "pull"
first" (a conflict), and the pull that follows writes the file back
("Recreated <file> (was missing locally)"), so the delete is undone whenever
another device touched the note in between (Apple bumps a note's change tag
for more than text edits). The engine:

- `push` moves such a note to Recently Deleted anyway, against the live
  record's change tag. The remote change is in the trashed note,
  recoverable for ~30 days.
- `pull` leaves a tracked note whose file is missing deleted: it takes the
  remote record's change tag and text into tracking (the base copy), writes
  no file, and notes "<file>: deleted here and changed in iCloud since - the
  next push moves it to Recently Deleted". `restore <file>` brings the file
  back from that base copy, as before.
- Unless the note was moved or renamed here and not pushed yet: when an
  untracked file carries the note's `apple-note-id`, pull merges the remote
  change into that file (diff3 against the old base) and keeps tracking at
  the old path, so the next push pairs the move with a matching change tag.
  Without this the remote edit would be lost: the move would go up, and
  the moved file's older text after it.
- A pull never writes or moves a note over a file it doesn't track (a note
  made here and not pushed yet): new notes and folder relocations take the
  names already on disk as used and get a `" 2"` name instead.

Tests: tests/cmd_local_delete.rs.

### Bounded history, and previews that record none

History lives in `history/<record>/` (one JSON snapshot of a note's or
table's CloudKit text per version that pull or push saw) and
`history/<note>/epochs/` (one entry per run that changed the note, naming
the snapshot current for each of its records). icloud-md 0.6.2 keeps every
snapshot forever and parses every snapshot of a record to compare with (or
index) the last one; on a real vault one note with an 837 KB document grew
23 snapshots of ~1.1 MB in a week. The engine (`src/vault/history.rs`,
`epoch.rs`):

- Reads only the latest snapshot: the file names are listed and sorted
  (`<ms>-<seq>-<shortId>.json`, oldest first) and the last one parsed. The
  next `seq` is one past the highest in the names. Lookups by id (`diff`,
  `restore`, `find_epoch_by_id`) parse only files whose name carries the
  id's short id.
- Prunes on every capture (`prune_history`, only the note's directories):
  each record keeps its newest 20 snapshots (`HISTORY_KEEP_RECENT`) plus
  the newest of each UTC day within 30 days (`HISTORY_RETENTION_DAYS`,
  the window `icloud-findmy prune-history` uses); the note's epochs follow
  the same rule, and a snapshot a kept epoch names is kept, so every
  listed epoch still resolves. Files that aren't capture names are left
  alone.
- `status` and `push --dry-run` record nothing: planning runs inside
  `history::without_recording`, where `record_version` and `record_epoch`
  are no-ops (and no migration runs either).

Tests: tests/vault_history.rs, and the recorded scenarios `tiny-status` and
`tiny-push-dry-run` (their vaults hold no history).

### Push honours what was done to the files

Anything done to the vault's files is pushed as the same change made in
Notes (icloud-md 0.6.2 refuses most of these, some forever):

- A note file at the top level of the vault is created in (or moved to) the
  default folder, where Notes puts a note made outside any folder.
- A copy of a tracked note's file (it carries the same `apple-note-id`) is
  a new note; the create gives it its own id.
- Emptying a note moves it to Recently Deleted, as Notes discards a note
  left empty; the empty file goes with it.
- A note edited here and deleted elsewhere is kept as a new note (pull
  untracks it and drops its id) instead of delete/modify conflict markers.
- A remote change is merged in memory and a clean merge goes up in the
  same push (no "re-run push"); a move's edits go up right after the move;
  a rename within a folder sends nothing to iCloud.
- A note with attachments can be edited: the link pull wrote for each file
  attachment stands for it, as an embed marker does. Removing, changing or
  moving the link is still refused. Moving such a note to another folder
  takes its attachment files along.
- Folder directories: a new directory (even empty) becomes a folder; a
  folder directory renamed in place (its notes all turn up in one new
  directory beside it, or both are empty) retitles the folder record; a
  folder directory deleted moves its notes to Recently Deleted and then
  deletes the folder record (`records/modify` `delete`, deepest first),
  unless the private zone's changes since the last pull show a note or
  folder this vault doesn't know in it (`Refusal::FolderChangedRemotely`).
  The default folder and shared folders are never renamed or deleted.

Tests: tests/cmd_local_delete.rs, tests/cmd_push.rs (folder planning),
tests/vault_folders.rs, tests/doc_embed_push.rs.

### New notes listed without their text

Found in review (2026-09-29). In icloud-md 0.6.2's `pull` a note new to the
vault that the private `changes/zone` walk delivers without
`TextDataEncrypted` is counted in `skippedNewUnsyncable` and skipped, while
the new `syncToken` is still saved - so it is never added until it changes
again. (Shared zones already look such notes up and hold back the whole
zone, carrying its old token.) The engine handles it in
`cmd::pull::backfill_new_note_bodies`, run after both fetches:

- Every live Note in the private listing that has no string
  `TextDataEncrypted` and is not tracked is looked up by id (one private
  `records/lookup`, merged in with `merge_looked_up_records`, then
  `inline_asset_bodies` - the shared backfill's steps). A note the lookup
  fills is added like any other.
- If any still has no text, it is skipped and counted as before, a warning
  says so ("Skipped N new note(s) that came through without their text, even
  when looked up - this vault's sync token was kept where it was, so the next
  pull will look for them again"), and `state.json` keeps the previous
  private `syncToken`. Chosen over remembering the record names in state:
  no new state field, and the next walk simply delivers the note again. The
  cost is that everything else in that window is delivered again too and
  re-applied (a clean note is rewritten with the same text and reported as
  updated), until the text arrives or the note is deleted.
- Tracked notes listed without text get a warning; the file is left alone.

`clone` had the same gap (such a note counted in `skippedUndecodable` and the
listing's `syncToken` saved past it, so a later `pull` does not see it
unless it changes). Since 0.1.2 the engine closes it the same way, with the
same helper (nothing is tracked yet, so every live private Note without text
is looked up):

- A note the lookup fills is written like any other.
- If any still has no text, it is skipped and counted in
  `skippedUndecodable` as before, a warning says so ("Skipped N note(s) that
  came through without their text, even when looked up - no sync token was
  saved for your own notes, so the first pull will look for them again (and
  re-read the rest)"), and `state.json` gets no `syncToken` - there is no
  previous one to keep. The first pull then walks the private zone from
  scratch, as clone did: every note is delivered again and re-applied (the
  cost the pull case accepts), and the note goes through pull's lookup
  above.
- Shared zones already behaved: the fetch looks such notes up, and a zone
  still missing text is skipped with its own warning and no token, so the
  first pull fetches it from scratch.

Tests: tests/cli_differential.rs `bodyless_pull_*` with the scenarios
`bodyless-pull` and `bodyless-pull-unfilled`: one private lookup of the
note, and then either the note added (lookup has the text) or the previous
token kept and a warning (lookup still without text). For clone,
`bodyless_clone_*` with the scenarios `bodyless-clone` and
`bodyless-clone-unfilled`: the note written and the token saved, or no
`syncToken` and a warning. tests/cmd_clone.rs covers the clone followed
by a pull that walks from scratch and adds the note, and a shared zone still
without text.

### Note documents compressed with flate2

icloud-md sends `zlib.deflateSync(raw)` (Node's bundled Chromium zlib), and
the engine first carried a hand port of that compressor to match it byte for
byte. `doc::text::compress_note_document` now uses flate2 (the zlib-rs
backend): still a zlib stream (RFC 1950 header, not gzip), at the default
level 6. For some inputs its bytes can differ from Node's, which CloudKit
doesn't care about: Apple accepts any valid zlib or gzip stream, and the
decoder reads both. Every request body the recorded scenarios send came out
unchanged.

Tests: tests/doc_deflate.rs (round trips over every real fixture and a
synthetic corpus; the container is zlib).

### Attachments: one lookup per zone, downloads overlapped

icloud-md 0.6.2 resolves a note's embeds when it reaches the note: one
`records/lookup` for its Attachment records, a second for their Media
records, then each file downloaded in turn (icloud-session fsyncs every
one). A pull or clone delivering N notes with attachments sent 2N lookups.
The engine (`vault::attachments::AttachmentRecords`,
`cloudkit::client::DownloadQueue`):

- Before the notes are applied, `AttachmentRecords::prefetch` decodes the
  embeds of every note the run will apply (pull skips the ones already at
  their change tag, as its loop does) and looks up all of a zone's
  Attachment records in one `records/lookup` walk, then all their Media
  records in a second: two walks of up to 200 names a request per zone. A
  name is asked for once; one the prefetch missed is looked up when its
  note needs it.
- Downloads go through a `DownloadQueue`. Over the live transport (one
  icloud-session `Session`, whose ureq agent is shared behind an `Arc`) they
  run on 4 background threads while the notes are processed; a failure
  surfaces at the next queued download, and the queue is drained (its first
  failure returned) before pull moves or removes any attachment file and
  before state is written. A destination already queued is fetched once.
  Over a transport without `Transport::shared_downloader` (the recorded
  scenarios' `ReplayTransport`, the test mocks) each download runs when
  queued, in order, so request logs stay deterministic.

File names, link paths and state entries are computed exactly as before,
in the same order. Recorded: `attach-clone` and `attach-pull` (two notes,
one file attachment each) send 2 lookups instead of 4.

Tests: tests/cli_differential.rs `attach_*`, tests/cloudkit_downloads.rs.

### Shared zones: the shared-database listing resumes

icloud-md 0.6.2 lists the shared database from scratch (`changes/database`
with no `syncToken`) on every pull and walks every shared zone
(`changes/zone` from its stored token). The engine stores where the listing
left off in an optional state.json key, `sharedDatabase`:
`{syncToken, zones: [{zoneName, ownerRecordName}], listedAt}` (`listedAt`:
ms of the last listing from scratch). `Database::fetch_shared_note_records_since`:

- With a cursor less than a day old (`SHARED_FULL_LISTING_INTERVAL_MS`),
  `changes/database` resumes from its token. Zones it lists are walked;
  zones it marks `deleted` or `purged` leave the known list; the cursor's
  other zones are unchanged since the token and are not walked (unless no
  zone sync token is stored for one, which is walked from scratch). An
  unchanged zone is still returned, with no records and its stored token -
  what an incremental walk of it would have come back with - so pull keeps
  its token, its sharer home and folders, and counts it live: a tracked
  note is untracked as no longer shared only when its zone is gone from
  the listing, as before.
- The cursor is listed before any zone is walked, so a zone that changes
  after its token was taken is reported by the next listing.
- A skipped zone (ZONE_NOT_FOUND, or notes still without text) keeps the
  cursor at its previous token (none after a first listing), so the next
  listing reports that zone again and it is retried; the zone list is
  updated.
- From scratch: when there is no cursor (older vaults, `clone`'s first
  listing; a malformed `sharedDatabase` reads as none), when it is a day
  old, and when the incremental request fails with an HTTP error or an
  answer without a `zones` array (an expired or unknown token).

CloudKit's documentation doesn't spell out how an incremental shared
listing reports a share the owner stopped (it should be a `deleted` or
`purged` zone, as in CloudKit's native API). Hence the daily listing from
scratch: a revocation an incremental listing missed is caught within a day,
and until then the zone's notes simply stay tracked (its `changes/zone`, if
it is walked, answers ZONE_NOT_FOUND, which was already handled).

Recorded: `shared-pull-unchanged` (resumed, nothing changed: no shared
`changes/zone` at all), `shared-pull-revoked` (the zone reported deleted:
its note untracked), `shared-pull-token-rejected`, `shared-pull-old-state`
and `shared-pull-stale-cursor` (from scratch). Every pull scenario now sends
the stored token.

Tests: tests/cli_differential.rs `shared_pull_*`,
tests/cloudkit_shared_zones.rs (changed, token-less and deleted zones, a
skipped zone holding the cursor), tests/vault_state.rs.
