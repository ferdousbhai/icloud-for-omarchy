# Notes (icloud-notes)

Apple Notes for Omarchy. Your notes live as plain Markdown files in
`~/Documents/Notes` — same folders as in Apple Notes — and sync
both ways with iCloud. (`~/Documents` is your documents folder,
`XDG_DOCUMENTS_DIR`, wherever that is.)

**Upgrading from an earlier release:** the notes folder used to be
`~/Documents/icloud-notes`. The first start after the update (the app,
`icloud-notes`, or the background sync) renames it to `~/Documents/Notes`
in place, once; nothing is copied or downloaded again. An empty
`~/Documents/Notes` is replaced. A `~/Documents/Notes` of your own (not a
notes vault) stops the move: Notes keeps using `~/Documents/icloud-notes`
and says so in a banner until you move that folder away and restart. If
both are vaults, neither is touched: Notes uses `~/Documents/Notes` and
logs a warning naming the old folder, for you to merge by hand. While a
Notes window of the earlier release is still open, the move waits for a
later start.

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
([notes-sync/](../notes-sync/README.md)), a Rust engine originally derived
from [icloud-md](https://github.com/coddingtonbear/icloud-md) (see
[Credits](#credits)), that syncs the folder of Markdown files with iCloud
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
QR sign-in lapses within hours of going unused. All your notes download into `~/Documents/Notes`,
one Markdown file per note with the title as its first line, just like
in Notes (the sync engine clones icloud-session's account; from a
terminal, `icloud-notes clone`). A download that stops part way keeps what
arrived, and the app syncs at once to fetch the rest (from a terminal,
`icloud-notes pull`). If the vault is ever missing while the computer is
signed in (a reinstall, say), the app downloads it again on its own,
without asking.

## Everyday use

- **Browse**: folders on the left, notes in the middle (newest first,
  with title, preview line, and date). The search box above the list
  searches every note; picking a result jumps to it. With Recall installed,
  notes related by meaning follow the text matches (see the main README).
  Right-click a folder to rename or delete it. Window and pane sizes are remembered.
- **Edit**: edits save on their own once you pause typing, and when you
  switch notes or close the window; `Ctrl+S` forces a save when a guardrail has flagged the
  edit. `Ctrl+N` starts a note. The first line is the title: edit it
  to rename the note. As in Typora, headings, emphasis, links and
  checklists are styled as you type, their Markdown marks show only on
  the line you are editing, and the file stays plain Markdown on disk. Bold/italic/link buttons (`Ctrl+B`/`Ctrl+I`/`Ctrl+K`),
  a checklist toggle (`Ctrl+Enter`), and PDF export (saved next to the
  note) are in the toolbar. `Ctrl++` and `Ctrl+-` zoom the note's text
  (remembered), `Ctrl+0` resets it. The arrow keys walk the panes like
  a file browser: Up/Down pick a folder or note (opening it as they
  go), Right/Enter step deeper into the note list and then the editor,
  Left/Escape step back out, and Home/End/PageUp/PageDown jump inside
  a list. The window follows the active Omarchy theme.
- **History** shows past versions of the current note with diffs,
  read-only. It keeps a note's newest 20 versions, plus one a day for
  the last 30 days; older ones are dropped as new ones arrive. Throwing away a note's local edits is a deliberate terminal
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
- Each sync is one run of the sync engine (push, then pull, over one
  connection), and the note list is re-read once afterwards, only when
  the sync changed something. Without a network the status line just
  says Notes is offline and syncs again later; that is not an error.
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
  change the note is copied as it was to `.icloud-notes/conflict-backups/`
  in the notes folder, which is never synced.
- Edits made in the app are pushed sooner, about 20 seconds after you
  stop making them, so a burst of typing becomes one push. Nothing waits
  for a click.
- What keeps this safe is icloud-notes-sync itself: a note it cannot push safely
  (an attachment link changed or moved, a reordered table, an unresolved conflict) is refused,
  not mangled, and a deleted note moves to Recently Deleted in iCloud
  (recoverable for ~30 days). Small badges in the note list warn you:
  brand-new notes, unresolved conflicts, notes the push refused, and notes
  changed on another device.
- **Sync status** (the toolbar's sync icon) shows what is pending — what
  would be created, updated, moved, or deleted, plus anything refused and
  why — with **Sync now** to sync at once instead of waiting. **Sync log**
  holds the details.
- A sync never deletes most of your notes by itself. When more than three
  note files, and most of them, are gone from the folder (a folder moved,
  a disk not mounted, another sync tool's mistake look just like deleting
  them), nothing is pushed and a banner says how many notes that would
  delete. Put the files back and syncing carries on; **Show them** lists
  them, and **Delete them…** moves them to Recently Deleted in iCloud
  once you confirm (the default keeps them). From a terminal:
  `icloud-notes push --dry-run`, then `icloud-notes sync --yes`.
- Apple ends a web session now and then. Syncing then pauses and a
  banner offers **Sign in**: icloud-session opens Apple's window, which
  skips 2FA for a browser you trusted; syncing resumes on its own once
  you are signed in. Apple refusing a session icloud-session still holds
  (`session_rejected`) pauses syncing the same way. The same banner
  appears a few days before a sign-in lapses, and after a sign-in too
  short to last. A sign-in or sign-out in another iCloud app (or with
  `icloud-session sign-in` in a terminal) shows up here at once.
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
Apple refused is reported to icloud-session just as the app does. Without
a network it logs one line and exits 0; the next run tries again.

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
icloud-notes attach Groceries receipt.png [--push]  # add a file to a note
icloud-notes resolve NOTE --all local|remote|both   # or --choices local,both,...
icloud-notes sync [--yes] | pull | push [--dry-run] [--yes] | clone
icloud-notes history NOTE | diff NOTE REF | restore NOTE --yes
```

While the Notes window is open it holds the vault, so a command that
changes a note is refused (`vault_busy`): make the change in the window, or
quit it. Every command, the output modes (text on a terminal, JSON when
piped), the exit and error codes and the JSON shapes are in the
[command-line reference](https://github.com/ferdousbhai/icloud-for-omarchy/blob/main/docs/CLI.md).

## Your files

Each note is one `.md` file. Editing or deleting a file with any other
program counts as editing or deleting the note: the next sync pushes the
edit, or moves the note to Recently Deleted in iCloud (even if another
device changed it in the meantime). A small ID block at the top of every file
links it to its iCloud original — don't delete it, or the next push
will treat the note as a brand-new one. Extra notes you add there
(tags, aliases) stay on your machine and never upload. Downloaded
images and files live in `attachments/` folders next to their notes. A note
with attachments can be edited and moved like any other, as long as the
links to its attachments stay as they are. To add an attachment in the
window, click the paperclip, drop a file on the note, or paste a copied
file or picture (a screenshot) into it. From a terminal, run
`icloud-notes attach NOTE FILE`, or do what it does by hand: put the file
in the `attachments/` folder next to the note and link it from the note the
way the others are linked, `![photo.png](attachments/photo.png)` for a
PNG, JPEG, HEIC, TIFF, GIF or WebP and `[plan.pdf](attachments/plan.pdf)`
for any other file. The next sync uploads it.

To remove an attachment, delete its link from the note: the next sync
removes it from the note in iCloud, on all your devices, and deletes the
file from `attachments/`. Because that cannot be undone from here, the
window never saves such an edit on its own: it stays in the editor until
you press Ctrl+S and confirm "Save anyway?". From a terminal, run
`icloud-notes detach NOTE FILE --yes` (FILE is the attachment's name as
`icloud-notes read NOTE` lists it); `icloud-notes write` refuses text that
drops an attachment's link unless you pass `--yes`. A link that is still
there but changed or moved is refused by the push, as before.

### Syncing the vault with Syncthing

The notes folder can be shared with another device (a phone, say) by
[Syncthing](https://syncthing.net): a note edited there is pushed by the
next sync here, like any other edit to its file. Two things to know:

- When both sides change a note before Syncthing has carried it over,
  Syncthing keeps one version under the note's name and the other as a
  conflict copy, `<name>.sync-conflict-<date>-<time>-<device>.md`. Such a
  copy is never synced to iCloud (it would otherwise go up as a second
  note): **Sync status** in the window, `icloud-notes push --dry-run` and
  every push list it with a warning until you have merged what you want
  from it into the note and deleted it. The window's note list shows it
  too, with a "Syncthing copy, not synced" badge (`icloud-notes list
  --flag conflict-copy` from a terminal): open it to read and copy from
  it. It does not get in the way of editing the note it copies.
  Syncthing's own files (`.stfolder`, `.stversions`, `.syncthing.*.tmp`)
  are ignored too, as every file or folder whose name starts with a dot is.
- Keep this machine's sync state off the other device. Put these lines in
  the folder's `.stignore`:

  ```
  .icloud-notes
  .icloud-md*
  .*.tmp
  ```

  `.icloud-notes` holds the state, merge bases and history (`.icloud-md`
  and `.icloud-md.bak-<time>` are what an older version left of them), and
  `.*.tmp` are files half written.

## Limitations

- Packages are built for x86_64 only. The app itself would build on
  Apple Silicon Omarchy; the release script just doesn't cross-build yet.

- You can add an attachment to a note from here (files up to 100 MB), and
  remove one, but neither on a note someone else shared with you: do that
  in Apple Notes. An attachment can't be moved to another place in its
  note from here. The text around it can be edited. A file added here
  goes up without the preview and text recognition Apple's own devices
  make for the attachments they add.
- A note icloud-notes-sync can read but not safely write back opens
  read-only, with the reason under its title: content it cannot reproduce
  exactly. Edit it in Apple Notes; the changes still sync here.
- A note edited daily for years grows large, because Apple keeps every
  edit ever made to it; past a size, iCloud stores its text as a separate
  file. Such a note is edited here like any other.
- Folder directories sync like folders in Notes: renaming one renames the
  folder in iCloud, deleting one moves its notes to Recently Deleted and
  deletes the folder (unless another device put something in it since the
  last pull), and a new directory, even an empty one, becomes a folder.
  Both are in the folder's right-click menu, and plain `mv`/`rm`/`mkdir`
  work the same way.
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
by Adam Coddington, which Notes ran directly until icloud-notes-sync, first
a Rust port of it, took over (and moved into this package); the engine has
gone its own way since. The vault keeps the engine's state in
`.icloud-notes/`. A vault from an older engine or from icloud-md keeps it in
`.icloud-md/`: Notes reads it there, and its first sync moves it to
`.icloud-notes/`, keeping a copy of the old directory as
`.icloud-md.bak-<time>` in the notes folder (delete it once you're happy).
After that, an older engine or icloud-md refuses the vault as written by a
newer version.

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing), which also covers the package-signing
key. Releases are tagged `v<version>`: one suite version for every package.
The `notes-v*` and `notes-sync-v*` tags are historical. The sync engine ships
inside icloud-notes.
