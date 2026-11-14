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
network error. Its data holds both results plus vault-info's answer, a
failed half's error included (no separate error object); the process exits
with the worse half (`Response::exit`).
icloud-session's `Offline` and `Network` errors are the errors `offline` and
`network`, its `Service` and `KeyringUnavailable` the errors `session_service`
and `keyring_unavailable` (all exit 5, with a hint) rather than `internal`
(exit 1). The run exits 3 if either half did, else the higher half's code.

In a machine mode (`--json`, `--quiet`, piped), progress goes to stderr as
`icloud-notes:progress:...` lines
(`fetch:N`, `process-start:N`, `process:I/N`, `process-done`). Engines
before layout 4 said `icloud-md:progress:...`; the app accepts both.

### Notes whose text is a separate asset

icloud-md 0.6.2 skips notes whose body lives in `TextDataAsset` (one user
note was ~844 KB: a page of text and years of edit history, which the CRDT
never drops). The engine reads and writes them.

Reading is upstream PR #29 (ferdousbhai, "Fetch the text of notes too large
to store it inline"): `cloudkit::client` requests `TextDataAsset`
(`note_desired_keys`) and `Database::inline_asset_bodies` puts the
downloaded bytes where every reader expects them, in `TextDataEncrypted`,
at the end of every zone walk, after the shared lookup backfill and in
`lookup_records` (what push edits). A failed download fails the fetch.
Always on: every scenario records `TextDataAsset` in its `changes/zone`
`desiredKeys`.

Writing is CloudKit's own asset upload, measured against a real account on
2026-11-11 (`Database::upload_asset`): `assets/upload` with
`{zoneID, tokens:[{recordType, recordName, fieldName}]}` answers an upload
URL; the bytes POSTed there (no cookies needed, `application/octet-stream`)
answer `{singleFile:{fileChecksum, size, wrappingKey, referenceChecksum,
receipt}}`; that object is the field's value in `records/modify`. An update
built by `build_note_update_fields` for such a note echoes its
`TextDataAsset` beside the new inline text, which is how
`Database::update_records` (and `update_records_independently`) knows to
upload the text and send the new asset in its place; the result gets the
text back inline. An upload that fails, stores a different size or gives no
receipt sends no update. A note stays in the form iCloud keeps it in: one
stored inline is never moved to an asset from here.

Up to 0.10.1 such a note was read-only ("is so large that Apple keeps its
text in a separate file, which can't be written back yet"); a vault's state
may still carry that reason, which `read_clone_state` drops.

Moving such a note to the trash or to another folder leaves its text alone,
sending neither `TextDataAsset` nor the inlined `TextDataEncrypted`
(`doc::encode::build_note_relocation_fields`). Emptying the asset, as the
update for an ordinary note does, leaves the record with no text, which
iCloud refuses ("Note record must have either TextDataEncrypted or
TextDataAsset").

A push that ran but had a change refused by iCloud returns its result as
before and exits 1 (`PushResult::failures`); in `sync` that half is
`{ok:false, exit_code:1, result, lines, error}` with the error
`push_rejected`.

Tests: tests/cloudkit_asset_body.rs (reading, and the upload on update);
the `asset-*` scenarios cover clone, a failed download, status, push (with
its upload) and pull end to end; tests/doc_encode.rs the trash and move
fields; tests/cmd_sync.rs the exit code.

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
untracked). The engine handles it in the fetch:

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
- `push` no longer backs this up with a refusal. A file that carries the
  `apple-note-id` of a tracked note whose own file is still present is a
  copy, and a copy is a new note, as a duplicated note is in Notes: the
  create gives it its own id (tests/cmd_push.rs,
  `copy_with_original_in_place_is_a_create`,
  `byte_identical_twin_is_a_create`). So nothing downstream catches a twin:
  the engine has to not write one, which is what the dedupe above and the
  next section are for.

Tests: tests/cloudkit_dedupe.rs (same page, across pages, timestamps vs page
order, differing tags, tombstone, private vs shared plus the lookup
backfill, a shared zone listed twice), and the scenario `dup-clone`
(`dup_clone_writes_a_repeated_record_once`: both pages fetched, and the same
vault/stdout as `tiny-clone`).

### A run that fails or is killed part way

A note file carries its id from the moment it is written, and push creates
a file whose id state.json doesn't track as a new note. So what a run did
has to reach state.json even when the run doesn't end well, or the next
push duplicates those notes in iCloud. Notes stops a running sync when its
window closes, so a killed run is ordinary.

- `pull` that fails writes the notes it applied (old sync tokens, so the
  rest is listed again), and saves what it has applied every 25 notes while
  the layout moves nothing (`checkpoint_state`). A failure while moving
  notes to their folders gets to that write too.
- `push` writes state.json after every entry it executes (an unchanged
  state is not rewritten), not only at the end or on an error.
- `clone` that fails after writing notes writes state.json for them, with
  no sync tokens: the directory is a clone from then on, and the first pull
  lists everything and adds the rest. One that fails before writing any
  note leaves no state. A clone killed outright leaves files and no state;
  the clone after it finds, in each directory it writes to, the files that
  carry the id of the note it is about to write, and takes one that reads
  exactly as it would write it for that note's file instead of writing
  `X 2.md` beside it. A file with the id that reads differently (edited
  since, or the note changed in iCloud) is left alone and the note written
  beside it; push then creates that file as a note of its own, so neither
  text is lost.

- `pull` does the same for a note new to the vault: an untracked file in
  the note's directory that carries its id and reads exactly as the pull
  would write it (what a pull killed before a save, or one that failed
  between writing the file and recording it, left) is taken for the note's
  file instead of `X 2.md` being written beside it.
- `push` (the first half of the sync that follows such a pull in the
  Notes app) leaves such a file alone: an untracked file whose
  `apple-note-id` no note here is tracked under is looked up first, and
  when iCloud has that note (not deleted, not in Recently Deleted) it is
  not created but left for the next pull, with a warning (so is one whose
  lookup answers an error). Created, the note would be in iCloud twice.
  After the pull has taken the file for the note, or written the note
  beside it because the file was edited since, the file is tracked or a
  copy, and a copy is created as a note of its own as before. An id
  iCloud doesn't have is still pushed as a new note.
- A pull that lays out a folder new to the vault (or a folder's new name)
  first lists the directories it is about to make in state.json's
  `pendingDirs`, and the state it saves once every note is where the
  layout puts it (or a checkpoint, which is only saved then) takes the key
  out. A pull killed in between leaves directories, and new notes in them,
  that the state names no folder for; read by a push, each would become a
  new folder in iCloud beside the one the pull was laying out. A push that
  finds `pendingDirs` sends nothing (a warning says why) until a pull has
  finished. An engine without it keeps the key and ignores it.
- A move whose request iCloud took before the push was cut off leaves the
  note tracked at its old file with the old change tag. The next push
  finds the record changed since, which refuses a move; but when iCloud
  has the note in the folder the file moved to, with the text it was last
  synced with (`push::moved_there_unchanged`), the move is done: only the
  file's tracking (and any edit made with the move) is brought up to date.
- A note whose attachment downloads a failed run may not have finished
  keeps its attachments' entries without their checksum, and forgets its
  change tag (`cmd::redo_downloads`): the next pull applies it again and
  fetches each file again under the name it has.
- A download never replaces a file in `attachments/` that no attachment
  tracks (a file put there to be attached): pull takes those names as used,
  and the download gets `name 2.ext`
  (`cmd::pull::claim_untracked_attachment_names`). The exception is what a
  killed pull left: a file linked from a note file that is itself untracked
  and carries an id nothing tracks is fetched again under its name, so that
  the note file reads as the pull would write it and is adopted (above).
  Whether a file there is byte-identical to the download is not compared:
  the name is chosen before the download runs.

- A run killed mid-write leaves a temporary file: the engine's own
  `.<pid>.<n>.tmp` beside a note, the state or a base copy, and
  icloud-session's `.<name>.<pid>.<n>.tmp` of a download in a folder's
  `attachments/`, which can be as large as the attachment and which a
  file-sync tool would carry to other devices. Every pull and every push
  (not a preview: they hold the vault's lock) starts by removing those
  whose process is gone (`vault::local::remove_stale_temporaries_in_vault`)
  from the vault's top directory, every layout directory and its
  `attachments/`, the state directory and `base/`. Only regular files with
  exactly those names: the pid and the counter plain decimal numbers with
  no leading zero, the pid one Linux can hand out; a download's name only
  in `attachments/`; never a `.syncthing.*.tmp`. The names carry nothing
  else to tell them by, so a hidden file a user names exactly that way in
  `attachments/` (with the number of no running process) is still taken
  for one.

- `push` announces a create before sending it: the record name (a new
  note's, drawn then; a new folder's, drawn by the plan) goes into
  state.json's `pendingCreates` (`{recordName: {file, sharedZoneOwner?,
  folder?: {title, parentRecordName?}, attachments?: [{attachmentRecordName,
  mediaRecordName, file}]}}`), which is written before the
  request and cleared once the entry's requests are answered. A push killed
  in between (iCloud has the record; the file has no id and nothing tracks
  it) leaves the entry. The next push, before it reads the vault
  (`cmd::push::settle_pending_creates`), looks the names up: a note that
  exists is tracked at its file (the announced one, or the untracked file
  already carrying the id) and its id stamped, and the attachments created
  with it (one atomic request) are tracked at their files, so that their
  links stand for them; a folder that exists is
  recorded; one iCloud doesn't have was never created and is created by the
  plan as usual. A lookup that answers an error fails the push
  (`cloudkit_request_failed`): the answer decides between one note and two.
  A pull that ran in between (by itself, or as the pull half of a sync
  whose push failed on that lookup) has added the note as one new to the
  vault, in a file of its own (`X 2.md`); the announced file then takes
  the note over and the pulled file, when it still reads as pulled, is
  removed, so that neither is left to be created as a second note. The
  attachments that pull downloaded for it under second names
  (`name 2.ext`) are tracked at the files the note was made with again,
  and the second copies removed. `status` and `push --dry-run` settle the
  same way in memory and write nothing.
- An edit that adds attachments to a note is announced the same way,
  under the note's own record name (`{file, attachments}`, the note's
  file): the request is atomic, so when the next push finds one of the
  announced `Attachment` records in iCloud, all of it went through. The
  attachments are tracked at their files, and when the note in iCloud
  reads exactly as its file does, the note is taken to be at that version
  and the file becomes its base copy (`adopt_pushed_text`): nothing is
  refused, sent or downloaded again. A file edited again since (or a note
  changed in iCloud too) stays at its old version, and the next pull
  merges the two, fetching the attachment under the name it has. An
  engine without this reads such an entry as a create of a note it
  already tracks at that file, and does nothing with it.
  `pendingCreates` is a top-level key an engine without it keeps and
  ignores (it would create the note again, as before).

Tests: tests/crash_points.rs runs the real binary over icloud-session's
mock session against a fake iCloud that keeps what it is sent, kills it
(`std::process::abort`) at every crash point it reaches and at each time
it reaches one, carries on with the same command (or a sync) and a sync,
and checks that no note, attachment or folder was created twice, nothing
the user kept was deleted or untracked, no edit was lost and state.json
reads back. The points are in `src/crash.rs`: one right after every durable
write (state.json, note files, base copies, history snapshots and epochs,
attachment downloads, renames, removals, an atomic write's temporary file,
`pendingCreates` written and cleared) and after every `records/modify` and
asset upload, and in a debug build only (`ICLOUD_NOTES_SYNC_CRASH_AT=
<point>[:<n>]`; `ICLOUD_NOTES_SYNC_CRASH_TRACE=<file>` lists the points a
run reaches). Also tests/cmd_pull_interrupted.rs, tests/cmd_push_interrupted.rs (a
failed and a killed push), tests/cmd_push_guards.rs (a push killed right
after iCloud created the note or the folder), tests/cmd_clone.rs
(`a_second_clone_over_an_unfinished_one_adopts_the_files_it_left`,
`a_second_clone_leaves_a_left_file_that_was_edited_alone`,
`a_clone_that_fails_part_way_tracks_what_it_wrote`).

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
  names already on disk as used and get a `" 2"` name instead. So do the
  attachment files of a note moved to another folder, in that folder's
  `attachments/`; the note's link to a file renamed that way is rewritten
  whole, as pull would write it for the new name (link text included: a
  link with the old text and the new target is one push refuses as
  "changed or moved"), and no other link is touched (`x.png` renamed
  leaves a link to `x.png.pdf` alone). The rename itself refuses a name that is taken
  (`renameat2` with `RENAME_NOREPLACE`, `vault::local::rename_no_replace`),
  so a file a file-sync tool delivers between the look and the rename is
  not replaced either.

Tests: tests/cmd_local_delete.rs, tests/vault_folders.rs.

### What a push never takes on trust

- An error is not "gone". `records/lookup` answers per record; a record it
  answers an error for other than `NOT_FOUND` is one iCloud said nothing
  about. A delete, an update, a move, a folder rename and a folder delete
  whose record came back that way are refused for this run
  (`Refusal::DeleteLookupFailed`, `Refusal::LookupFailed`), and so is an
  edit whose tables or removed attachments did
  (`PrepareRefusal::LookupFailed`): read as gone, the note would be
  untracked (and stay in iCloud), or an attachment's records left out of
  its removal.
- A copy is not the note. A missing note is paired with the one untracked
  file of its name only when that file doesn't carry another tracked note's
  id: such a file is that note's copy, and taking it for the missing note
  would overwrite a note being deleted with another note's text.
- The file may have been saved again. A remote change merged into a local
  edit is planned from the file as the plan read it (`RemoteMerge`'s
  `planned_frontmatter`, `planned_text`). When the entry runs and the file
  no longer reads so, nothing is sent or written for it: the outcome is
  `skipped` ("changed here while it was being pushed"), not a failure, and
  the next push plans it afresh. Such an update is sent by itself, right
  after that check, never in the batched write.

Tests: tests/cmd_push_guards.rs, tests/cmd_push_attach.rs
(`a_removal_whose_lookup_answered_an_error_is_refused`),
tests/cmd_local_delete.rs (`push_refuses_a_delete_whose_lookup_answered_an_error`).

### A note that is gone from iCloud

A tracked note that iCloud lists as deleted (or that a listing from scratch
no longer has), and a note whose owner stopped sharing it, are both gone
for this account (`cmd::pull::handle_remote_deletion`):

- The note's file is removed when its text is still the last synced one
  (the base copy). The file is the tracked one or, when that is missing, the
  untracked file that carries the note's id: where the note was renamed or
  moved to here without a push since. Up to 0.11.1 such a renamed file was
  left, with an id nothing tracked, and the next push created it: a note
  deleted on the phone came back as a new one.
- A file with edits made here is kept and no longer tracked. Of a deleted
  note it loses its `apple-note-id` and the next push creates it as a new
  note, edits and all (as before).
- Of a note no longer shared, an unedited file is removed like a deleted
  note's (up to 0.11.1 it was left in place, and the next push uploaded it,
  with a new folder named after the sharer's record id, into the user's own
  notes: someone else's note, after they took access away). An edited file
  is kept with its id, and state.json lists the id under `unshared`. Push
  leaves an untracked file that carries a listed id alone, warns ("is your
  edited copy of a note that is no longer shared with you, so it is not
  synced"), and makes no folder of a directory that is new only because of
  such files. Removing the `apple-note-id` line makes the file a note of
  the user's own. `unshared` is a top-level key an engine without it keeps
  as it is; an id leaves the list when its note is tracked again.

Tests: tests/cmd_local_delete.rs (the last five), tests/cmd_pull.rs
(`clean_absent_note_is_removed_like_a_tombstone`,
`absent_note_with_local_edits_is_kept_as_a_new_note`), the scenario
`shared-pull-revoked`.

### Most of the vault's files gone

Deleting a note's file deletes the note, so a vault whose note files went
missing while `.icloud-notes/` stayed (a folder moved aside, a disk not
mounted, another sync tool's mistake) would have every note moved to Recently
Deleted and every folder record deleted by the next push, which the Notes
app runs by itself. A push now counts the deletions it planned (notes whose
file is gone or emptied and that it would move to Recently Deleted; one
iCloud no longer has, or has there already, is only untracked and does not
count) and refuses the whole run, before sending anything,
when they are more than 3 and more than half of the account's own notes
tracked (`push::too_many_deletions`, the rule icloud-reminders' folder
applies to a list; notes someone else shares are never deleted from here,
so they don't count): the error `too_many_deletions`, exit 2, with a hint. `push --yes` and
`sync --yes` (`PushOptions::allow_deletes`) push it. `status` and `push
--dry-run` list the deletions as before and add a warning. In a `sync` it is
the push half's error; the pull still runs (it never writes a deleted note's
file back).

Tests: tests/cmd_push_guards.rs
(`a_push_that_would_delete_most_of_the_vault_is_refused_until_told_to`,
`every_own_note_gone_is_refused_whatever_is_shared_with_the_account`,
`deleting_some_notes_is_not_refused`).

### A tracked note this build can no longer decode

A tracked note whose text arrives in a form the decoder can't read
(`UnsyncableReason::Undecodable`) is still a note in iCloud. Up to 0.11.1 a
pull stopped tracking it and left its file in place (icloud-md 0.6.2 does the
same); the file carries the note's `apple-note-id`, which push then finds
tracked by nothing, so the next push created it as a new note: a second copy,
with the older text, of a note that was still there. Now the note stays
tracked: its file and base copy are left as they are, its entry takes the
record's change tag and a read-only reason (vault-info's `readOnlyReason`),
and pull warns. An edit made here is refused by push (the live record is not
editable); deleting the file still moves the note to Recently Deleted; a
later version that decodes is applied like any other change. `PullSummary`'s
`droppedUnsyncable` stays in the result and is always 0.

Tests: tests/cmd_pull_interrupted.rs
(`a_tracked_note_that_became_undecodable_stays_tracked_and_is_not_pushed_as_new`).

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
- A snapshot or epoch file that can't be read (cut short by a crash of a
  version that wrote it in place, or damaged since) is left out of a
  listing instead of failing it: `history` lists the rest and names the
  file in a notice (`notices` in its data, a warning in text mode), and a
  lookup by id (`diff`) passes over it.
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
  attachment stands for it, as an embed marker does. Changing or moving
  the link is still refused. Moving such a note to another folder takes
  its attachment files along.
- A link of that kind to a file in the note folder's `attachments/` that
  is no attachment yet adds the file to the note ("Adding an attachment",
  below).
- A file attachment's link gone from the note entirely removes the
  attachment from the note ("Removing an attachment", below).
- Folder directories: a new directory (even empty) becomes a folder; a
  folder directory renamed in place (its notes all turn up in one new
  directory beside it, or both are empty) retitles the folder record; a
  folder directory deleted moves its notes to Recently Deleted and then
  deletes the folder record (`records/modify` `delete`, deepest first),
  unless a walk of the whole private zone finds a record this vault doesn't
  track in it: a note or folder another device put there since the last pull
  (`Refusal::FolderChangedRemotely`), or a locked or unreadable note, which
  pull never tracks, so the directory looked empty
  (`Refusal::FolderHoldsUnsyncableNotes`).
  The default folder and shared folders are never renamed or deleted.

Tests: tests/cmd_local_delete.rs, tests/cmd_push.rs (folder planning),
tests/cmd_push_folder_delete.rs,
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

A lookup answers per record. `NOT_FOUND` means the record is gone, and its
slot becomes an embed marker. Any other per-record error says nothing of the
record (`Database::lookup_records_and_failures`), so a note that needs it
(`AttachmentRecords::lookup_failure`) is left exactly as it is this run - no
marker, its tracked attachments and their files kept, a new note not written
yet - with a warning naming it, and the run goes on. Its zone's sync token
stays where it was (a shared zone's: every shared token and the listing
cursor; in a clone, none is saved), so the next pull lists the note again.
Read as gone, the attachment's file would be deleted and the note, then at
its current change tag, never looked at again until it changed in iCloud.

File names, link paths and state entries are computed exactly as before,
in the same order. Recorded: `attach-clone` and `attach-pull` (two notes,
one file attachment each) send 2 lookups instead of 4.

Tests: tests/cli_differential.rs `attach_*`, tests/cloudkit_downloads.rs,
tests/cmd_pull_interrupted.rs (`an_attachment_lookup_error_*`).

### Adding an attachment

A note's text that links to a file the way pull links an attachment
(`[plan.pdf](attachments/plan.pdf)`, with a leading `!` for the image
types `is_image_uti` knows), where the file is in the note folder's
`attachments/` and is no tracked attachment, asks for that file to be
attached. This works for an edited note and for a new note's file.
`icloud-notes attach NOTE FILE` writes exactly that: a copy of the file and
the link.

What a file attachment is in iCloud (measured on a real account,
2026-11-11, for a PNG):

- a `Media` record, `parent` the note: `Asset` (the file), `FilenameEncrypted`
  (base64 of the name), `MinimumSupportedNotesVersion` 0, `Deleted` 0;
- an `Attachment` record, `parent` the note, whose record name is the
  attachment identifier (an uppercase UUID): `UTI`, `UTIEncrypted`,
  `TitleEncrypted` (the file name), `Media` and `Note` (references,
  `{recordName, action: "VALIDATE", zoneID: {zoneName: "Notes"}}`),
  `FileSize`, `CreationDate`, `LastModificationDate`,
  `MinimumSupportedNotesVersion` 0, `Deleted` 0, `Orientation` 0,
  `ImageFilterType` 0, the cropping quad as the whole image
  (`CroppingQuadTopLeftX` 0, `TopLeftY` 1, `TopRightX` 1, `TopRightY` 1,
  `BottomRightX` 1, `BottomRightY` 0, `BottomLeftX` 0, `BottomLeftY` 0) and,
  for an image, `Width` and `Height` in pixels;
- in the note's text, a U+FFFC whose attribute run (one unit long, with its
  paragraph's style) carries the attachment identifier and the type.

Apple's devices later add previews and summaries to the `Attachment`
record (`PreviewImages`, `OcrSummaryEncrypted`, ...). The engine sends none
of those (`doc::encode::build_media_create_fields`,
`build_attachment_create_fields`); the cropping quad and the image size go
up as JSON doubles, everything untyped like the engine's other writes.

Planning (`status`, `push --dry-run` and the first half of `push`) reads
nothing of the file but its metadata and sends nothing for it:

- `doc::embeds::plan_embed_representations` gives every attachment link
  that stands for no embed of the note a placeholder of its own
  (`new_links`), in the place of the link. A link inside a table or an
  embed marker is that embed's text. Something written like a link into
  `attachments/` that can't be read as one (its text over a line break, a
  space in its target) is refused rather than pushed as an ordinary link.
- `cmd::push::resolve_new_attachments` turns each into a `NewAttachment`
  or a refusal (`PrepareRefusal::NewAttachment`,
  `Refusal::CreateNewAttachment`). The file must be a regular file named
  directly in `attachments/` (`vault::attachments::resolve_new_attachment_file`:
  no `..`, no deeper path, no symbolic link, and the directory itself
  resolving inside the vault, the test the app's preview applies),
  non-empty and at most 100 MB (`MAX_NEW_ATTACHMENT_BYTES`), not an
  attachment of this or another note already, linked once, and linked
  exactly as pull would link it (`attachment_link_spellings`), since that
  spelling is what stands for the attachment from then on. The type comes
  from the extension (`doc::attach::uti_for_file_name`, `public.data` when
  it says nothing). A note in someone else's shared zone is refused.
- The record names (uppercase UUIDs) are drawn here, as a new folder's is,
  because the note's document names the attachment: the plan entry gets
  the remark "adds the attachment NAME".
- The document is built by `prepare_note_text_update`. The text edit is
  applied as if the new placeholders weren't there, under the existing
  rule that an edit may neither add nor remove a placeholder; then each new
  one is inserted at its offset by
  `doc::document::insert_attachment_placeholder` (one character of this
  replica, with its attachment run). Diffing the whole text instead could
  not tell a new placeholder from the one beside it, and would hand an
  existing attachment's character to the new one (a link appended after a
  note's last attachment did exactly that). Every check on the rebuilt
  document still runs, the embed check against the note's embeds plus the
  new ones in order. A new note's document marks its placeholders with
  `mark_attachment_placeholder` and is checked the same way.

Executing (`send_with_attachment_changes`): each file is read and uploaded
with `Database::upload_asset` for the `Asset` field of the `Media` record
it is about to become, and one `records/modify` with `atomic: true`
(`Database::modify_records_atomically`) carries the note's update (or
create) first, any table updates, then a `Media` and an `Attachment`
create per file. Such a note is left out of the batched non-atomic write.
If iCloud refuses any record nothing is applied: the entry fails with the
refusal of the record that caused it (the others answer `ATOMIC_ERROR`),
and state, base copy and files are as they were, so the next push tries
again. A failed upload fails the push like any failed request, before
anything is sent for the note; an uploaded asset no record names is
iCloud's to discard. On success `state.attachments` gets the entry pull
would have written (the file where it is, the upload's `fileChecksum`), so
the link is an existing attachment to the next plan, and the base copy and
history are written as for any pushed edit.

Not measured against a real account: `assets/upload` was measured for a
field of a record that exists (a Note's `TextDataAsset`), not for a record
the same push is about to create; that the untyped doubles are accepted;
that the cropping quad is wanted on a file that is no image; and that
Apple's apps show a record without the previews they add themselves.

Still refused: changing or moving the link of an attachment the note has
(removing it removes the attachment, next section); a link to a file that isn't there, or outside the folder's
`attachments/`; a file over 100 MB; a shared note. GIF and WebP files are
images like the rest (`com.compuserve.gif`, `org.webmproject.webp`), as
Apple Notes shows them; a link pulled before they were, written as a file's,
is still taken for its attachment.

Tests: tests/doc_attach.rs (types, image sizes, the records' exact JSON,
links, placeholders through the decoders), tests/cmd_push_attach.rs (the
upload and the exact atomic request over a fake server, a second
attachment, a failed upload, a refused request, every refusal, a new
note), and the scenarios `attach-push` and `attach-push-status`
(`attach_push_*`).

### Removing an attachment

The mirror image of adding one. A file attachment the vault tracks for a
note (`state.attachments`, `noteRecordName` the note) whose link is gone
from the note's text entirely is removed from the note by the next push.
`icloud-notes detach NOTE FILE --yes` writes exactly that: the note without
the link.

What Apple's apps do when an attachment is removed (measured on a real
account, 2026-11-13, removing one on an iPhone): the `Attachment` record
and its `Media` record both get `Deleted` set to 1 and keep every other
field (the `Media` record still has `Asset`, `FilenameEncrypted`,
`MinimumSupportedNotesVersion`; the `Attachment` record its `UTI`, `Media`,
`Note`, sizes, previews and the rest). Neither record is deleted outright.
The `Note` record stays `Deleted: 0`, with a new `ModificationDate` and
its text without the attachment's U+FFFC.

"Gone entirely" (`doc::embeds::plan_embed_representations`): the link pull
wrote is not found where it belongs, and no attachment link anywhere in the
text points at the file (`has_attachment_link_to`: with or without the
`!`, with any link text, so also the link of a GIF or WebP pulled before
those counted as images). The slot is then listed in
`EmbedRepresentation::removed_slots` and gets no placeholder in the
reconstructed text. A link to the file that is still somewhere, with
another text or in another place, is refused as before ("was changed or
moved ... put the link back as it was, or remove it entirely"): moving an
attachment is not something a delete and an insert of its character would
do faithfully. Only a tracked file attachment is removed this way; a table
whose block is gone, or any other embed whose marker is gone, is refused
as before.

Planning (`cmd::push::prepare_embed_candidate`), for `status`, `push
--dry-run` and the first half of `push`:

- A note in someone else's shared zone is refused
  (`PrepareRefusal::RemovedAttachment`): its attachments are the owner's.
- One `records/lookup` fetches the removed attachments' `Attachment`
  records and their `Media` records (the names state tracks), together
  with the tables the edit touches, for their change tags. A record that
  is gone, or deleted already (another device removed the attachment too),
  gets no operation. A record that is not an `Attachment`/`Media` record,
  or an `Attachment` whose `Note` names another note, refuses the push:
  nothing this vault only believes to be the note's attachment is marked
  deleted.
- The document is built by `prepare_note_text_update`. Each removed
  placeholder is deleted first, by itself, last one first
  (`doc::document::delete_attachment_placeholder`: the character at that
  offset must be a U+FFFC whose run carries that attachment's identifier;
  it is tombstoned by this replica the way `apply_text_edit` deletes any
  character, and its attribute run goes with it). The text edit is then
  applied against what is left, under the unchanged rule that an edit may
  neither add nor remove a placeholder, and the new attachments' are
  inserted last. Diffing the whole text instead could take the
  placeholder beside the removed one for it. The rebuilt document must
  decode to the embeds that stay plus the new ones, in order.
- The plan entry is an `update` with the remark "removes the attachment
  NAME" ("removes the attachments A, B"; after "adds the attachment ..."
  and a retitle's remark, joined with "; ").

Executing (`send_with_attachment_changes`): one `records/modify` with
`atomic: true` carries the note's update first, any table updates, then
per removed attachment an update of its `Attachment` record and of its
`Media` record setting `Deleted` to 1 and nothing else, each with the
change tag just looked up and its `parent` (`attachment_removal_ops`, the
one place that builds them), then the creates of any new attachments.
Such a note is left out of the batched non-atomic write. If iCloud refuses
any record nothing is applied, and state, base copy and files are as they
were: the next push tries again. On success
(`forget_removed_attachments`) the attachment leaves `state.attachments`
and its file is deleted from the folder's `attachments/` with
`safe_unlink`, as pull deletes the file of an attachment that disappeared
remotely (there is no trash for attachment files). The file stays when it
is still someone's: `attachments/` is shared by the notes of a folder, so
a file tracked for another attachment, or linked from another note file in
the same directory, is kept. A removal made along with a rename or move of
the note goes up with the edit that follows the move.

Pull: a later pull of the same change is a no-op. The note comes back at
the change tag the push stored, so it is skipped, and `Attachment` and
`Media` records are not among the record types a zone walk asks for. A
vault that pulls the change made elsewhere (another computer's push, or
Apple Notes) applies the note's new text, which has no slot for the
attachment, so its entry is stale: untracked, file deleted. Pull used to
skip attachment handling for a note with no embeds at all, which left the
file and the entry of a note's last attachment behind; it now runs
whenever the note still tracks an attachment or a table.

The Notes app guards the other end (`notes/src/notesbackend.cpp`
`removedAttachments`, `saveWarning`): text that drops the link of an
attachment the note lists is never autosaved; Ctrl+S asks "Save anyway?"
with "This removes the attachment NAME from this note on all your
devices.". `icloud-notes write` refuses such text without `--yes`
(`usage`, exit 2), as does `detach`.

Not measured against a real account: that an update carrying only
`Deleted` (no other field) is accepted for these two record types (Apple's
own client may send more fields along); and whether Apple's client also
changes the note's `FirstAttachmentThumbnail`/`FirstAttachmentUTIEncrypted`
beyond what `build_note_update_fields` already sends for every note update
(both `null`).

Still refused: changing or moving an attachment's link; removing from a
shared note; removing a table or any other embed.

Tests: tests/doc_embed_push.rs (gone entirely against changed or moved,
several at once, with an addition, other embeds), tests/doc_attach.rs (the
placeholder's delete through the decoders), tests/cmd_push_attach.rs (the
exact atomic request over a fake server, the pull after it and another
vault's pull, removals with edits and an addition, a changed link, a
shared note, a file another note still uses, a refused and a failed
request, records deleted already, a record that is not the note's, a
renamed note), and the scenarios `detach-push-status`, `detach-push`,
`detach-push-clean` and `detach-pull` (`detach_push_*`).

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
its note removed), `shared-pull-token-rejected`, `shared-pull-old-state`
and `shared-pull-stale-cursor` (from scratch). Every pull scenario now sends
the stored token.

Tests: tests/cli_differential.rs `shared_pull_*`,
tests/cloudkit_shared_zones.rs (changed, token-less and deleted zones, a
skipped zone holding the cursor), tests/vault_state.rs.
