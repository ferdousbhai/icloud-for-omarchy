# Live push test

`push_itest.sh` performs **real writes** to a real iCloud Notes account and
checks every push against a fresh clone of the account made by the same
binary into a separate scratch vault, so what iCloud holds is read back with
nothing the pushing vault remembers. It is never run by `cargo test`.

```bash
# from the repository root
cargo build --release -p icloud-notes-sync
ICLOUD_NOTES_SYNC_LIVE=1 ICLOUD_NOTES_SYNC_ITEST_ACCOUNT=<dsid> notes-sync/tests/live/push_itest.sh
```

The binary signs in through icloud-session. A run takes a few minutes.

## Containment

- Every vault is a fresh scratch clone under
  `~/.cache/icloud-apps-test/push-live/run-<runId>/`; the script refuses a
  work root under `~/Documents`.
- All writes go into one folder, `icloud-notes-sync-itest`, created by the
  engine's own folder-create path on the first run. The run refuses to start if
  that folder holds a note without an `itest-` prefix.
- Every note it creates is titled `itest-<runId> ...`.
- Before every real push, `status --json` and `push --dry-run --json` are
  logged and `guard.py plan` refuses the push unless the two plans agree,
  every entry is `ready` (the one exception: the dry-run that checks a file
  with conflict markers is refused, where `conflict` is required), every file (and `previousFile` / `pendingRename`)
  is inside the folder with this run's prefix (directly, or in a subfolder
  named with the prefix), every folder entry is the containment folder or
  such a subfolder, and every non-create entry is a note id this run created
  (a create may carry one too: a copy of this run's note). The expected entry kinds are asserted
  too where they are known.
- Only this run's notes are ever deleted. The folder is left in place.

CloudKit intermittently lists a record twice. The clone dedupes the listing
and never writes a second copy, and push refuses such a file anyway
(docs/DESIGN.md §1). `guard.py dedupe` still deletes untracked
byte-identical copies from every scratch vault before any plan is made; it
should always report 0.

## Scenarios

1. Create the folder and a note (heading, checklist, bold/italic, link), push.
2. Edit it (append, bold a line, tick a checklist item), push, pull, status clean.
3. Create a second note, push; retitle it, push; rename its file, push.
4. Two vaults: edits on different lines merge cleanly on pull; edits to the
   same line produce conflict markers, which the plan refuses to push until
   they are resolved.
6. Two vaults edit different lines; the second pushes without pulling, and
   the push merges and uploads in one go.
7. A copy of a note's file is pushed as a new note with its own id, then
   deleted.
8. Emptying a note moves it to Recently Deleted.
9. A subfolder of the containment folder (named with the run's prefix) is
   made empty, renamed, gets a note moved in with an edit, and is deleted
   with that note; a fresh clone after each step shows iCloud followed.
5. Delete both notes, push; fresh clones no longer hold them. (Runs last.)

After each push, a fresh clone must hold the same notes as the pushing vault
(matched by `apple-note-id`) with identical bytes. Names are not compared:
in in-body title mode neither push nor pull renames a retitled or renamed
note's file, while a fresh clone names it by title.

Logs (every command's JSON, stderr, exit code and timing in `summary.tsv`)
stay in the run directory; nothing is cleaned up.

| Variable | Meaning |
| --- | --- |
| `ICLOUD_NOTES_SYNC_LIVE=1` | Required. |
| `ICLOUD_NOTES_SYNC_ITEST_ACCOUNT` | Apple ID or dsid to clone as. Required. |
| `ICLOUD_NOTES_SYNC_ITEST_FOLDER` | Containment folder (default `icloud-notes-sync-itest`). |
| `ICLOUD_NOTES_SYNC_ITEST_WORKROOT` | Where run directories go (default `~/.cache/icloud-apps-test/push-live`). |
| `ICLOUD_NOTES_SYNC_BIN` | Binary under test (default `target/release/icloud-notes-sync` at the workspace root). |
