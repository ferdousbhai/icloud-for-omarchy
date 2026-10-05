# notes-sync and icloud-md 0.6.2

notes-sync (`icloud-notes-sync`) is the sync engine behind icloud-notes: a
Rust port of icloud-md 0.6.2 (MIT, Adam Coddington), whose clone, pull, push,
status, history, diff and restore it reproduces byte for byte - the same exit
codes, output, CloudKit requests and vault files (`layoutVersion: 3`,
`generator: "icloud-md 0.6.2"`). Apple calls go through icloud-session
(`session/`) instead of icloud-md's own sign-in.

That parity was established against icloud-md itself (its results recorded
over the same cassettes and inputs). The suite no longer runs icloud-md: the
recorded scenarios in `tests/differential/` (see its README, run from
`tests/cli_differential.rs`), the golden corpora in `tests/golden/` and the
real-fixture goldens are now regression tests recorded from this crate
(re-recorded with `ICLOUD_NOTES_SYNC_REGEN=1`), so the port is free to
diverge. The places it deliberately differs are listed below.

## 1. Deliberate differences from icloud-md 0.6.2 (after parity)

### The vault lock, and vault-info (not in icloud-md)

icloud-md has no lock. The port takes the one icloud-notes holds
(`src/cmd/lock.rs`): clone, pull, push and restore take an exclusive flock on
`$XDG_RUNTIME_DIR/icloud-notes-<FNV-1a 64 of the vault's canonical path>.lock`
(beside the vault without a runtime directory), the path icloud-notes'
`NotesBackend::lockPath()` computes too. Non-blocking: another run or a
background sync is waited for (`--wait SECS`, 30 by default), the Notes
window, which holds it while open, not at all; busy is `vault_busy`, exit 1.
status, history, diff, push --dry-run and vault-info only read and take no
lock (a layout 2 → 3 migration, the one write they can make, is idempotent).
A caller already holding the lock (the app) hands its locked descriptor down
as `ICLOUD_NOTES_LOCK_FD`; it is used only if it is open on this vault's lock
file and holds the lock.

`vault-info` (JSON: title mode, default folder, tracked notes with their
read-only reasons and base copies, the state and lock files) is what the app
reads instead of parsing state.json.

Upstream PR #29 (ferdousbhai, "Fetch the text of notes too large to store it
inline", open and unmerged as of 2026-09-29; fork branch
`fetch-asset-note-bodies`, commits 768bcb3 and 10fa221 in the icloud-md clone)
is not in 0.6.2, so 0.6.2 skips notes whose body lives in TextDataAsset (the
user has one: an ~833 KB pinned note). Port it once the read path matches 0.6.2:
request TextDataAsset, inline the downloaded bytes, mark such notes unpublishable
("is so large that Apple keeps its text in a separate file, which can't be
written back yet"). Differential tests then run with a flag that turns it off.

Done: `cloudkit::client` (`note_desired_keys`, `Database::inline_asset_bodies`
at the end of every zone walk and after the shared lookup backfill; a failed
download fails the fetch) and `doc::decode::classify_note_record` (the PR's
reason string). Always on (the switch back to stock 0.6.2 that the parity
scenarios used is gone; every scenario now records `TextDataAsset` in its
`changes/zone` `desiredKeys`). PR tests ported in
tests/cloudkit_asset_body.rs; the `asset-*` scenarios cover clone, a failed
download, status, push and pull end to end. Live: a fresh clone of the real account matches
the installed PR #29 build byte for byte (bar `generator`).

### Records listed twice by CloudKit (not in 0.6.2)

Live testing (2026-09-29, a ~118-note account) found about 1 clone in 9
writing one note twice: a second byte-identical file under a uniquified
name (`X 2.md`), only one of the two tracked in `state.json`, so the next
`push` would create the other as a duplicate note. icloud-md 0.6.2 does exactly the same. Cause: a `changes/zone` walk
occasionally lists the same record on two pages (the run logs show the fetch
total one higher than the clones before and after it with no note created in
between, and `written` one higher, for icloud-md and the port alike);
`fetchZoneNoteRecords` concatenates pages and `clone` materializes every
occurrence (and `state.json` keeps only the last one, so the first file is
untracked). The port deviates in two places:

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
  note"). 0.6.2 treats such a file as a copy and creates it. Moves (the
  tracked file gone, one claimant) and ambiguous claims are unchanged.

Tests: tests/cloudkit_dedupe.rs (same page, across pages, timestamps vs page
order, differing tags, tombstone, private vs shared plus the lookup
backfill, a shared zone listed twice), tests/cmd_push.rs
(`copy_with_original_in_place_is_refused_as_a_duplicate`,
`byte_identical_twin_from_a_double_clone_is_refused`), tests/cmd_refusals.rs
(port-only row), and the scenario `dup-clone`
(`dup_clone_writes_a_repeated_record_once`: both pages fetched, and the
same vault/stdout as `tiny-clone`).

### Local deletes, renames and new files win over a pull (not in 0.6.2)

Deleting a note's file is a delete, as deleting the note in Notes is. In
0.6.2 it is only when nothing changed remotely since the last pull: push
refuses with "changed remotely since the last pull - run "pull" first"
(push.ts L967, a conflict), and the pull that follows writes the file back
("Recreated <file> (was missing locally)", pull.ts), so the delete is undone
whenever another device touched the note in between (Apple bumps a note's
change tag for more than text edits). The port:

- `push` moves such a note to Recently Deleted anyway, against the live
  record's change tag (the `Refusal::DeleteChangedRemotely` variant is
  gone). The remote change is in the trashed note, recoverable for ~30 days.
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
  names already on disk as used and get a `" 2"` name instead. 0.6.2 only
  avoids tracked names and overwrites the file.

Tests: tests/cmd_local_delete.rs.

### Bounded history, and previews that record none (not in 0.6.2)

0.6.2 keeps every snapshot forever, and `recordVersion` / `recordEpoch`
parse every snapshot of a record to compare with (or index) the last one.
On a real vault one note with an 837 KB document grew 23 snapshots of
~1.1 MB in a week. The port (`src/vault/history.rs`, `epoch.rs`):

- Reads only the latest snapshot: the file names are listed and sorted
  (`<ms>-<seq>-<shortId>.json`, oldest first) and the last one parsed. The
  next `seq` is one past the highest in the names (the count, as 0.6.2
  numbers them, while nothing was pruned). Lookups by id (`diff`,
  `restore`, `find_epoch_by_id`) parse only files whose name carries the
  id's short id.
- Prunes on every capture (`prune_history`, only the note's directories):
  each record keeps its newest 20 snapshots (`HISTORY_KEEP_RECENT`) plus
  the newest of each UTC day within 30 days (`HISTORY_RETENTION_DAYS`,
  the window `icloud-findmy prune-history` uses); the note's epochs follow
  the same rule, and a snapshot a kept epoch names is kept, so every
  listed epoch still resolves. Files that aren't capture names are left
  alone. The on-disk format is unchanged, and icloud-md reads it.
- `status` and `push --dry-run` record nothing: planning runs inside
  `history::without_recording`, where `record_version` and `record_epoch`
  are no-ops. 0.6.2 records the looked-up note's snapshot while planning.

Tests: tests/vault_history.rs, and tests/cli_differential.rs
`previews_icloud_md_records_history_and_the_port_does_not` with the
scenarios `tiny-status` and `tiny-push-dry-run` (`portDeviation:
previews-write-nothing`): icloud-md's vault is asserted to hold one history
snapshot; the port's is asserted to be icloud-md's without it (its state
directory the unchanged `tiny-clone` one), with the same exit, stdout,
requests and mtimes.

### New notes listed without their text (not in 0.6.2)

Found in review (2026-09-29). In 0.6.2's `pull` (`src/commands/pull.ts`
L294-306) a note new to the vault that the private `changes/zone` walk
delivers without `TextDataEncrypted` is counted in `skippedNewUnsyncable`
and skipped, while the new `syncToken` is still saved - so it is never
added until it changes again. (Shared zones already look such notes up and
hold back the whole zone, carrying its old token.) The port deviates in
`cmd::pull::backfill_new_note_bodies`, run after both fetches:

- Every live Note in the private listing that has no string
  `TextDataEncrypted` and is not tracked is looked up by id (one private
  `records/lookup`, merged in with `merge_looked_up_records`, then
  `inline_asset_bodies` - the shared backfill's steps). A note the lookup fills is added like any other.
- If any still has no text, it is skipped and counted as before, a warning
  says so ("Skipped N new note(s) that came through without their text, even
  when looked up - this vault's sync token was kept where it was, so the next
  pull will look for them again"), and `state.json` keeps the previous
  private `syncToken`. Chosen over remembering the record names in state:
  no new state field, and the next walk simply delivers the note again. The
  cost is that everything else in that window is delivered again too and
  re-applied (a clean note is rewritten with the same text and reported as
  updated), until the text arrives or the note is deleted.
- Tracked notes listed without text keep 0.6.2's behaviour (a warning, the
  file left alone).

`clone` has the same gap in 0.6.2 (`src/commands/clone.ts`): such a note is
counted in `skippedUndecodable` and the listing's `syncToken` is saved past
it, so a later `pull` does not see it unless it changes. Since 0.1.2 the port
closes it the same way, with the same helper (nothing is tracked yet, so
every live private Note without text is looked up):

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
