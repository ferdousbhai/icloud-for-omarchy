# Live push test

`push_itest.sh` performs **real writes** to a real iCloud Notes account and
checks every push against an independent oracle: a fresh clone made by the
installed `icloud-md`. It is never run by `cargo test`.

```bash
# from the repository root
cargo build --release -p icloud-notes-sync
ICLOUD_NOTES_SYNC_LIVE=1 ICLOUD_NOTES_SYNC_ITEST_ACCOUNT=<dsid> notes-sync/tests/live/push_itest.sh
```

The port signs in through icloud-session. The oracle `icloud-md` keeps its
own session under `~/.config/icloud-md`, so sign it in to the same account
first. A run takes a few minutes.

## Containment

- Every vault is a fresh scratch clone under
  `~/.cache/icloud-apps-test/push-live/run-<runId>/`; the script refuses a
  work root under `~/Documents`.
- All writes go into one folder, `icloud-notes-sync-itest`, created by the
  port's own folder-create path on the first run. The run refuses to start if
  that folder holds a note without an `itest-` prefix.
- Every note it creates is titled `itest-<runId> ...`.
- Before every real push, `status --json` and `push --dry-run --json` are
  logged and `guard.py plan` refuses the push unless the two plans agree,
  every entry is `ready` (the one exception: the dry-run that checks a file
  with conflict markers is refused, where `conflict` is required), every file (and `previousFile` / `pendingRename`)
  is directly inside the folder with this run's prefix, and every non-create
  entry is a note id this run created. The expected entry kinds are asserted
  too where they are known.
- Only this run's notes are ever deleted. The folder is left in place.

A fresh icloud-md 0.6.2 clone can (intermittently, when CloudKit lists a
record twice) write one note twice, once as an untracked byte-identical copy,
which its next push would then create as a new note. The port dedupes the
listing and never writes the copy, and its push refuses such a file anyway
(docs/PORT_PLAN.md §1). `guard.py dedupe` still deletes such copies from
every scratch vault before any plan is made; with the port it should always
report 0.

## Scenarios

1. Create the folder and a note (heading, checklist, bold/italic, link), push.
2. Edit it (append, bold a line, tick a checklist item), push, pull, status clean.
3. Create a second note, push; retitle it, push; rename its file, push.
4. Two vaults: edits on different lines merge cleanly on pull; edits to the
   same line produce conflict markers, which the plan refuses to push until
   they are resolved.
5. Delete both notes, push; fresh clones no longer hold them.

After each push, a fresh icloud-md clone and a fresh port clone must hold a
byte-identical test folder, and the pushing vault must hold the same notes
(matched by `apple-note-id`) with identical bytes. Names are not compared
with the pushing vault: in in-body title mode neither push nor pull renames a
retitled or renamed note's file (icloud-md 0.6.2 behaves the same), while a
fresh clone names it by title.

Logs (every command's JSON, stderr, exit code and timing in `summary.tsv`)
stay in the run directory; nothing is cleaned up.

| Variable | Meaning |
| --- | --- |
| `ICLOUD_NOTES_SYNC_LIVE=1` | Required. |
| `ICLOUD_NOTES_SYNC_ITEST_ACCOUNT` | Apple ID or dsid to clone as. Required. |
| `ICLOUD_NOTES_SYNC_ITEST_FOLDER` | Containment folder (default `icloud-notes-sync-itest`). |
| `ICLOUD_NOTES_SYNC_ITEST_WORKROOT` | Where run directories go (default `~/.cache/icloud-apps-test/push-live`). |
| `ICLOUD_NOTES_SYNC_BIN` | Binary under test (default `target/release/icloud-notes-sync` at the workspace root). |
| `ICLOUD_NOTES_SYNC_ORACLE` | Oracle command (default `icloud-md`). |
