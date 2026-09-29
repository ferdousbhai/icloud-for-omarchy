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
- The sync engine, `icloud-notes-sync`: a command-line tool that syncs
  a folder of Markdown files with iCloud Notes (a Rust port of
  [icloud-md](https://github.com/coddingtonbear/icloud-md); see
  [Credits](#credits)).
- `icloud-session`, which owns the iCloud sign-in for Notes and the other
  iCloud apps. It is a small D-Bus service
  (`io.github.ferdousbhai.ICloudSession`, started on demand) with Apple's
  own sign-in window; it keeps the account signed in, and
  icloud-notes-sync takes the session it syncs with from it.
  Without it Notes cannot sign in, and shows no sign-in warnings.

The package depends on both, and the installer adds their signed
repositories; when building from source, install them too.

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
key, adds the signed `[icloud-notes]` repository, and installs the app:

```bash
curl -fsSL https://ferdousbhai.com/icloud-notes/install.sh | sudo bash
```

Updates then arrive with `omarchy update`. The script is
[`install.sh`](install.sh) in this repo, and the copy the one-liner runs
is the one attached to the latest release, verified with it; read it
first if you like. It also installs an Omarchy `pre-refresh-pacman` hook
so `omarchy refresh pacman` keeps the repository.

To uninstall: `omarchy pkg drop icloud-notes icloud-notes-sync icloud-session`
(drop `icloud-session` only if no other iCloud app uses it; removing the
package also removes the background-sync timer). Then, for each of the
`icloud-notes`, `icloud-notes-sync` and `icloud-session` repositories, remove
`/etc/pacman.d/<name>.conf`, its `Include` line in `/etc/pacman.conf`, and
`~/.config/omarchy/hooks/pre-refresh-pacman.d/<name>`.

To build and run from source instead:

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
in Notes (icloud-notes-sync clones icloud-session's account,
`icloud-notes-sync clone --account <dsid>`). If the vault is ever missing while the computer is
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
  step (`icloud-notes-sync restore`, see
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
  open side by side the same way.
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
the checkout, after `./bin/build`, install them for your user with the
service pointed at the binary you built:

```bash
mkdir -p ~/.config/systemd/user
sed "s|^ExecStart=.*|ExecStart=$PWD/build/icloud-notes --sync|" data/icloud-notes-background.service \
  > ~/.config/systemd/user/icloud-notes-background.service
cp data/icloud-notes-background.timer ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now icloud-notes-background.timer
```

## Your files

Each note is one `.md` file. A small ID block at the top of every file
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
cd ~/Documents/icloud-notes
icloud-notes-sync restore "<note file>"
```

## Credits

The sync engine started as [icloud-md](https://github.com/coddingtonbear/icloud-md)
by Adam Coddington, which Notes ran directly until icloud-notes-sync, its
Rust port, took over. Vaults are icloud-md vaults (the `.icloud-md/` state
directory is unchanged), so a vault cloned with either tool works with the
other.

## Releasing

Releases are cut from a checkout with the package-signing key in its
keyring, no CI involved:

```bash
bin/release 0.2.0
```

That runs the tests, tags `v0.2.0`, builds the package with `makepkg` from
`pkgbuild/PKGBUILD`, signs it and the repository database with the key
whose fingerprint `install.sh` pins, and publishes everything as the
GitHub release for the tag, which is what `releases/latest/download` in
`install.sh` resolves to; `install.sh` itself is attached too, and the
one-liner runs that copy. A release counts as shipped only once
`bin/verify-release` has run the public one-liner in a clean Arch
container and found that version installed; otherwise `bin/release`
deletes the release and the tag.

The `add_signed_repo` function in `install.sh` is shared verbatim with the
Ghost installer (`install.sh` in ferdousbhai/ghost, published as a release
asset and served from ferdousbhai.com/ghost/install.sh), and both
repositories pin its hash in their tests: change it in both places, and both
hashes, together.

### The signing key

One key signs both projects' packages; its fingerprint is pinned in both
installers and it lives only in the releasing machine's keyring, protected
by a passphrase. Losing it would break the trust chain on every machine
that installed from these repositories, so keep an encrypted backup
somewhere off this machine:

```bash
gpg --armor --export-secret-keys 35C47A06567940B6796B4D0F9B3C7BDF85268B31 \
  | gpg --symmetric --armor --output package-signing-key.backup.asc
```

Restoring is `gpg --decrypt package-signing-key.backup.asc | gpg --import`.

To rotate the key: generate the new one, publish one release from each
project signed with the old key that also ships the new public key as
`<name>-signing-key.asc`, update the pinned fingerprint in both
installers and the tests, then sign the next releases with the new key.
Machines that installed earlier pick up the new key by re-running the
one-liner, which is idempotent.
