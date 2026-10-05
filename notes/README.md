# Notes (icloud-notes)

Apple Notes for Omarchy. Your notes live as plain Markdown files in
`~/Documents/icloud-notes` — same folders as in Apple Notes — and sync
both ways with iCloud.

## Requirements

- An Apple ID with **Advanced Data Protection turned off** (off by
  default) and **Access iCloud Data on the Web turned on**. This is set
  once per Apple ID, not per device: on iPhone/iPad go to Settings →
  your name → iCloud → Advanced Data Protection (same path in macOS
  System Settings). Without this, nothing outside Apple's own apps can
  read your notes.
- `icloud-session`, which owns the iCloud sign-in for Notes and the other
  iCloud apps. It is a small D-Bus service
  (`io.github.ferdousbhai.ICloudSession`, started on demand) with Apple's
  own sign-in window; it keeps the account signed in, and
  icloud-notes-sync takes the session it syncs with from it.
  Without it Notes cannot sign in, and shows no sign-in warnings.

The package depends on icloud-session, and the installer adds its signed
repository; when building from source, install it too.

The sync engine comes in the package: `icloud-notes-sync`
([notes-sync/](../notes-sync/README.md)), a Rust port of
[icloud-md](https://github.com/coddingtonbear/icloud-md) (see
[Credits](#credits)) that syncs the folder of Markdown files with iCloud
Notes. It is installed off PATH, at
`/usr/lib/icloud-notes/icloud-notes-sync`, and run for you by the window,
the background sync and `icloud-notes`; up to icloud-notes 0.5.0 it was a
separate package, which this one now replaces. A build from source finds it
through `ICLOUD_NOTES_SYNC_BIN` (e.g.
`ICLOUD_NOTES_SYNC_BIN=$PWD/../target/debug/icloud-notes-sync` after
`cargo build -p icloud-notes-sync`), else in `/usr/lib/icloud-notes`, else
on PATH.

## Sign-in

Notes never signs in by itself, and icloud-notes-sync has no sign-in of
its own.
icloud-session holds the one Apple account on this computer; Notes reads
from it over D-Bus whether you are signed in, as whom, and when the
sign-in lapses, and follows its changes as they happen. **Sign in** asks
icloud-session to open its window; a sign-in there (or in any other
iCloud app) resumes syncing here. When icloud-notes-sync reports that Apple
refused the session, Notes tells icloud-session, which checks with Apple
before signing every app out. A sign-in made before icloud-session took
over the account is not carried over: sign in once more.

## Install

On Omarchy (or any Arch Linux), one command trusts the package-signing
key, adds the signed `[icloud-for-omarchy]` repository (all the iCloud
apps and what they share), and installs the app:

```bash
curl -fsSL https://ferdousbhai.com/icloud-notes/install.sh | sudo bash
```

Updates then arrive with `omarchy update`. The one-liner runs
`install-notes.sh` from the latest release: [`install.sh`](../install.sh)
at the root of this repository, set to install just this app. It also
installs an Omarchy `pre-refresh-pacman` hook so `omarchy refresh pacman`
keeps the repository, and replaces the `[icloud-notes]` repository
earlier releases used. See the root [README](../README.md#install) for
uninstalling.

Uninstalling (`omarchy pkg drop icloud-notes`, and `icloud-session` if
nothing else uses it) also removes the sync engine and the background-sync
timer.

To build and run from source instead (from `notes/`):

```bash
./bin/build
./build/icloud-notes
```

## First run

On first launch the **Link your Apple Notes** dialog opens — press
**Clone my notes**. If this computer is not signed in to iCloud yet,
icloud-session's window opens Apple's real sign-in; your password and
2FA stay on Apple's own pages. To stay signed in, use your Apple ID
and password rather than the iPhone QR code, tick **Keep me signed in**,
and click **Trust** when asked: that sign-in lasts about 30 days, while a
QR sign-in lapses within hours of going unused. All your notes download into `~/Documents/icloud-notes`,
one Markdown file per note with the title as its first line, just like
in Notes (the sync engine clones icloud-session's account; from a
terminal, `icloud-notes clone`). If the vault is ever missing while the computer is
signed in (a reinstall, say), the app downloads it again on its own,
without asking.

## Everyday use

- **Browse**: folders on the left, notes in the middle (newest first,
  with title, preview line, and date). The search box above the list
  searches every note; picking a result jumps to it. Right-click a folder
  to rename or delete it. Window and pane sizes are remembered.
- **Edit**: edits save on their own once you pause typing, and when you
  switch notes; `Ctrl+S` forces a save when a guardrail has flagged the
  edit. `Ctrl+N` starts a note. The first line is the title: edit it
  to rename the note. As in Typora, headings, emphasis, links and
  checklists are styled as you type, their Markdown marks show only on
  the line you are editing, and the file stays plain Markdown on disk. Bold/italic/link buttons (`Ctrl+B`/`Ctrl+I`/`Ctrl+K`),
  a checklist toggle (`Ctrl+Enter`), and PDF export (saved next to the
  note) are in the toolbar. The window follows the active Omarchy theme.
- **History** shows past versions of the current note with diffs,
  read-only. Throwing away a note's local edits is a deliberate terminal
  step (`icloud-notes restore`, see
  [If something looks wrong](#if-something-looks-wrong)), never a click.

## Syncing

Sync is automatic, like Notes:

- On launch, and whenever you switch to the window (at most once a
  minute), the app pushes whatever changed locally
  (including edits made by other programs, or while it was closed) and
  then pulls what changed in iCloud. Opening a note, clicking into it, or
  starting to type in it does the same when the last pull is over a
  minute old, and the note waits the few seconds until it is in, so you
  edit the latest copy. Edits made on both sides merge automatically
  when they don't overlap.
- While the window stays open it keeps checking: every minute while it
  is the active window, every 15 minutes while it is not. iCloud does not
  notify a web client of changes, so an edit on your phone shows up here
  within about a minute. After a failed pull it waits longer each time,
  up to 15 minutes, and goes back to every minute once a pull works.
- When they do overlap, the note opens on the two versions side by
  side, this computer's and iCloud's, with the lines that differ
  highlighted. Pick one for each change (or keep both) and the note
  syncs like any other edit; **Edit as text** shows the raw merge
  markers instead.
- If a note changes underneath edits you haven't saved yet (a pull, or
  another program writing the file), the app never saves over that
  change. Your edits merge with it line by line: edits to different
  lines land in the note on their own, and only lines both sides changed
  open side by side the same way. A note that still has an unresolved
  conflict is never merged into (that would nest one conflict in
  another): your edits go to a new note, "<title> (unsaved edits)", and
  the conflicted note stays as it is.
- A note whose conflict markers are garbled (nested or out of order)
  opens on a banner instead of the versions: **Keep this computer's
  text** drops only the marker lines and keeps every other line,
  **Use the last synced version** goes back to the text last synced with
  iCloud, and **Edit as text** shows the raw markers. Before either
  change the note is copied as it was to `.icloud-md/conflict-backups/`
  in the notes folder, which is never synced.
- Edits made in the app are pushed sooner, about 20 seconds after you
  stop making them, so a burst of typing becomes one push. Nothing waits
  for a click.
- What keeps this safe is icloud-notes-sync itself: a note it cannot push safely
  (attachments, a reordered table, an unresolved conflict) is refused,
  not mangled, and a deleted note moves to Recently Deleted in iCloud
  (recoverable for ~30 days). Small badges in the note list warn you:
  brand-new notes, unresolved conflicts, notes the push refused, and notes
  changed on another device.
- **Push…** shows a preview on demand — what would be created, updated,
  moved, or deleted, plus anything refused and why — and pushes on
  confirmation. **Pull** fetches now. **Sync log** holds the details.
- Apple ends a web session now and then. Syncing then pauses and a
  banner offers **Sign in**: icloud-session opens Apple's window, which
  skips 2FA for a browser you trusted; syncing resumes on its own once
  you are signed in. The same banner appears a few days before a sign-in
  lapses, and after a sign-in too short to last. A sign-in or sign-out
  in another iCloud app (or with `icloud-session sign-in` in a terminal)
  shows up here at once.
- New folders upload as real Notes folders.

## Background sync

While Notes is closed, a systemd user timer runs `icloud-notes --sync`
every 15 minutes (and two minutes after boot, and after a missed run
while the computer slept): the same push-then-pull the app does, with no
window. Edits made elsewhere reach this computer, and edits made here by
other programs reach iCloud, so a note is less likely to be hours stale
when you next edit it. It does nothing while the app is open (the app
syncs itself, and the two never run icloud-notes-sync at once), when nobody is
signed in to icloud-session, or before the notes are cloned. A sign-in
Apple refused is reported to icloud-session just as the app does.

The package turns it on for every user (from the next login; the installer
also starts it at once). To turn it off for yourself:
`systemctl --user mask --now icloud-notes-background.timer` (`unmask` undoes it).
What it did is in `journalctl --user -u icloud-notes-background`.

Built from source without the package, the units are not installed. From
`notes/`, after `./bin/build`, install them for your user with the
service pointed at the binary you built:

```bash
mkdir -p ~/.config/systemd/user
sed "s|^ExecStart=.*|ExecStart=$PWD/build/icloud-notes --sync|" data/icloud-notes-background.service \
  > ~/.config/systemd/user/icloud-notes-background.service
cp data/icloud-notes-background.timer ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now icloud-notes-background.timer
```

## Command line

`icloud-notes <command>` does from a terminal (or an agent) whatever the
window does, with no window and no display: the same rules (read-only
notes, the save checks, trash semantics, the version picker), through the
same code. `icloud-notes help` lists the commands and `icloud-notes
COMMAND --help` describes one, with its JSON.

```bash
icloud-notes status                       # vault, sign-in, lock holder, flagged notes
icloud-notes list [--folder F] [--flag conflict]
icloud-notes read "Groceries"             # a path in the vault, an apple-note-id, or a unique title
icloud-notes search milk
icloud-notes new "Groceries" --body "bread" [--folder Notes] [--push]
icloud-notes write Groceries --append --body "- [ ] jam" [--push]
icloud-notes rename|move|delete|toggle ...
icloud-notes resolve NOTE --all local|remote|both   # or --choices local,both,...
icloud-notes recover NOTE --strip|--synced          # unreadable conflict markers
icloud-notes new-folder|rename-folder|delete-folder ...
icloud-notes sync | pull | push [--dry-run] | clone
icloud-notes history NOTE | diff NOTE REF | restore NOTE --yes
```

- Every command takes `--json`: the result as JSON on stdout, an error as
  one JSON line on stderr, `{"error":{"code","message","exit_code","hint"}}`.
- Exit codes: 0 ok, 1 error, 2 sign-in required (`icloud-session
  sign-in`), 3 `push --dry-run` has changes or `diff` found differences,
  64 usage: the table every iCloud tool shares
  ([docs/CLI.md](../docs/CLI.md)).
- Commands that change the vault or sync take the vault's lock first, as
  the window and the background sync do. While the window is open it holds
  that lock, so they are refused at once (`vault_busy`): make the change in
  the window, or quit it. A background sync is waited for (30 s, or
  `--wait SECS`). Reading never needs the lock; `history`, `diff` and
  `push --dry-run` only read too. icloud-notes-sync takes the same lock
  itself, so running it directly obeys it as well.
- A change stays on this computer until the next sync (the window's, the
  background timer's, or `icloud-notes sync`); `--push` on any change runs
  one right after it.
- `delete`, `delete-folder` and `restore` ask on a terminal and need
  `--yes` otherwise. Nothing else ever asks.
- `--vault DIR` works on another vault (tests use a scratch one).

## Your files

Each note is one `.md` file. Editing or deleting a file with any other
program counts as editing or deleting the note: the next sync pushes the
edit, or moves the note to Recently Deleted in iCloud (even if another
device changed it in the meantime). A small ID block at the top of every file
links it to its iCloud original — don't delete it, or the next push
will treat the note as a brand-new one. Extra notes you add there
(tags, aliases) stay on your machine and never upload. Downloaded
images live in `attachments/` folders next to their notes and are
preview-only: notes with attachments can't be edited back to iCloud.

## Limitations

- Packages are built for x86_64 only. The app itself would build on
  Apple Silicon Omarchy; the release script just doesn't cross-build yet.

- Notes with images, audio, or file attachments are read-only upstream;
  you can't add attachments from here either.
- A note icloud-notes-sync can read but not safely write back opens
  read-only, with the reason under its title. A very large note is the
  usual case: Apple keeps its text in a separate file, which
  icloud-notes-sync reads but does not write back.
  Edit it in Apple Notes; the changes still sync here.
- Folders carry no id in iCloud, so a folder rename here becomes a new
  folder plus note moves on push, and a folder delete moves its notes to
  Recently Deleted; the old folder stays in Notes, empty, until you delete
  it there. Both are in the folder's right-click menu.
- Table edits mostly round-trip, but reordering rows/columns is
  refused — the push preview will tell you.
- Changes from other devices arrive within about a minute while the
  window is active, and right away when you switch to it, open a note, or
  start editing one (or press Pull). They don't arrive instantly like on
  the Mac app. While the app is closed, [background sync](#background-sync)
  catches up every 15 minutes.

## If something looks wrong

Open **Sync log** to see exactly what the last operation did. To throw
away local edits on one note and go back to the last synced copy:

```bash
icloud-notes restore "<note>" --yes
```

It takes the vault's lock, which the Notes window holds while it is open:
close Notes first.

## Credits

The sync engine started as [icloud-md](https://github.com/coddingtonbear/icloud-md)
by Adam Coddington, which Notes ran directly until icloud-notes-sync, its
Rust port, took over (and moved into this package). Vaults are icloud-md vaults (the `.icloud-md/` state
directory is unchanged), so a vault cloned with either tool works with the
other.

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing), which also covers the package-signing
key. Its tags are `notes-v<version>` (`notes-v0.3.8` was `v0.3.8` before
the move into this repository). Releasing icloud-notes releases the sync
engine too; the `notes-sync-v*` tags are from when it was a package of its
own.
