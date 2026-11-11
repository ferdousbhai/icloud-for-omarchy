# iCloud Drive

`~/iCloud Drive` kept in sync with iCloud Drive, both ways, over the shared
[icloud-session](../session/README.md) sign-in. There is no window and no
file command: it is a folder, so the file manager, a terminal, an editor or
an agent reads and writes it like any other. `icloud-drive` only reports on
the sync and turns it on and off.

```sh
icloud-drive start      # turn the sync on, now and at every login
icloud-drive status     # on or off, the last sync, problems, what is not synced
icloud-drive sync       # sync once now (--dry-run says what it would do)
icloud-drive stop       # turn it off; the folder and its files stay
```

The first sync downloads your whole Drive and uploads whatever the folder
already holds; files that are the same on both sides are left as they are.
After that it syncs every half minute while you are signed in.

## What happens when

- **A file changes on one side:** the other side gets it. A file edited
  here replaces the one in iCloud in place, so its history and sharing stay.
- **It changes on both sides:** both are kept. Yours is renamed
  `name (conflicted copy <computer> <date>).ext` and uploaded; iCloud's
  version takes the name.
- **A file is removed in iCloud:** it goes to the desktop's trash here.
- **A file is removed here:** it goes to Recently Deleted in iCloud, where
  it stays for 30 days. Nothing is ever deleted for good.
- **One side removes a file the other side edited:** the edit wins and the
  file comes back.
- **A rename or a move:** the same rename or move on the other side; the
  file is not sent again.
- **Many removals at once** (more than 20 and more than a quarter of what
  is in sync, or a folder emptied of everything): the sync stops and says so
  in `icloud-drive status`. Look with
  `icloud-drive sync --dry-run`, then allow them once with
  `icloud-drive sync --yes`.
- **The folder itself is missing** (an unmounted disk, say): nothing is
  synced, so its absence is never read as everything having been deleted.

## What is not synced

`icloud-drive status` lists these as it meets them.

- App folders (Pages, Numbers, Shortcuts and the like at the top of Drive).
- Packages: Pages, Numbers and Keynote documents and other bundles, which
  are folders that iCloud presents as one file.
- Hidden names (starting with a dot), links, and names iCloud does not take
  (containing `:` or a line break). A folder removed in iCloud goes to the
  trash here whole, with any hidden files in it.
- Two names in one folder that differ only by case: neither is synced.
- Folders shared with you by other people.

Requirements are the suite's: an Apple ID with Advanced Data Protection off
and Access iCloud Data on the Web on.

## How it works

Each run asks iCloud's change feed (CloudKit, container
`com.apple.clouddocs`) what changed since the last run. The feed names the
changed files and folders, not what changed, so the folders it names are
listed again through the Drive web API and everything else comes from the
last listing. Every six hours, and whenever the feed starts over,
everything is listed. A listing that is incomplete or reports a folder as
gone stops the run before anything is removed.

The folder is scanned, and the plan is made by a pure function
(`src/plan.rs`) from three views: what was in step after the last sync
(SQLite in `~/.local/state/icloud-drive/`), iCloud now, and the folder now.
Moves are found by item id in iCloud and by inode here. Each operation
records its result as it finishes, so a run that stops half way is picked
up by the next from wherever both sides stand.

An upload is first written as a form in `~/.cache/icloud-drive/`, so a large
file needs that much free space again while it goes up.

A replaced file is sent with the etag it had when listed; iCloud refuses the
replace if the file changed there meanwhile, and the next run keeps both.

Code: `src/remote.rs` (the web API, and what was measured about it),
`src/local.rs` (the folder, the trash), `src/state.rs`, `src/plan.rs` (the
rules, each with a test), `src/engine.rs`, `src/cli.rs`. `tests/sync.rs`
runs the engine end to end against an in-memory iCloud Drive that behaves
as the real one was measured to. `cargo run -p icloud-drive --example
scratch` makes a scratch folder in the real Drive for trying a sync
against one folder (`--top <id> --dir <dir> --state-dir <dir>`).
