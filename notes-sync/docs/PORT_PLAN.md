# notes-sync and icloud-md 0.6.2

notes-sync (`icloud-notes-sync`) is the sync engine behind icloud-notes: a
Rust port of icloud-md 0.6.2 (MIT, Adam Coddington), whose clone, pull, push,
status, history, diff and restore it reproduces byte for byte - the same exit
codes, output, CloudKit requests and vault files (`layoutVersion: 3`,
`generator: "icloud-md 0.6.2"`). Apple calls go through icloud-session
(`session/`) instead of icloud-md's own sign-in.

The differential suite that holds it to that lives in `tests/differential/`
(cassettes, scenarios and icloud-md's recorded results; see its README) and
runs from `tests/cli_differential.rs`. The only places it deliberately differs
are listed below.

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
reason string). `ICLOUD_NOTES_SYNC_ASSET_BODIES=0` restores stock 0.6.2; the
0.6.2 differential scenarios run with it, the `asset-*` scenarios compare with
the fork branch (see tests/differential/README.md). PR tests ported in
tests/cloudkit_asset_body.rs. Live: a fresh clone of the real account matches
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
(port-only row), and the differential scenario `dup-clone`
(`portDeviation: dedupe-records`): icloud-md's expectation is recorded as
usual and asserted to contain the duplicate; the port is asserted to send the
same requests and to produce icloud-md's `tiny-clone` vault/stdout/mtimes.

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
  `inline_asset_bodies` when asset bodies are on - the shared backfill's
  steps). A note the lookup fills is added like any other.
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
`bodyless-pull` and `bodyless-pull-unfilled` (`portDeviation:
look-up-new-bodyless-notes`): icloud-md's expectation is asserted to skip
the note, send no lookup and save the new token; the port is asserted to
send the same requests plus one lookup, and then either to add the note
(lookup has the text) or to keep the previous token with otherwise
icloud-md's summary and vault (lookup still without text).
For clone, `bodyless_clone_*` with the scenarios `bodyless-clone` and
`bodyless-clone-unfilled` (same `portDeviation`) assert the same way: the
note written and the token saved, or no `syncToken` with otherwise
icloud-md's summary and vault. tests/cmd_clone.rs covers the clone followed
by a pull that walks from scratch and adds the note, and a shared zone still
without text.
