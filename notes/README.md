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
- The sync tool: `npm install -g icloud-md` (needs Node.js 20+).

## Install

On Omarchy (or any Arch Linux), one command trusts the package-signing
key, adds the signed `[icloud-notes]` repository, and installs the app:

```bash
curl -fsSL https://ferdousbhai.com/icloud-notes/install.sh | sudo bash
```

Updates then arrive with `omarchy update`. The script is
[`install.sh`](install.sh) in this repo; read it first if you like. It
also installs an Omarchy `pre-refresh-pacman` hook so `omarchy refresh
pacman` keeps the repository.

To uninstall: `omarchy pkg drop icloud-notes`, then remove
`/etc/pacman.d/icloud-notes.conf`, its `Include` line in
`/etc/pacman.conf`, and `~/.config/omarchy/hooks/pre-refresh-pacman.d/icloud-notes`.

To build and run from source instead:

```bash
./bin/build
./build/icloud-notes
```

## First run

On first launch the **Link your Apple Notes** dialog opens — press
**Clone my notes** and pick how titles are stored (first line of the
file, or file name — this can't be changed later). A real Apple
sign-in window opens; your password and 2FA stay on Apple's own
pages. All your notes download into `~/Documents/icloud-notes`.

## Everyday use

- **Browse**: folders on the left, notes in the middle (newest first,
  with title, preview line, and date). The search box above the list
  searches every note; picking a result jumps to it.
- **Edit**: `Ctrl+S` saves, `Ctrl+N` starts a note, switching notes
  autosaves. Bold/italic/link buttons (`Ctrl+B`/`Ctrl+I`/`Ctrl+K`), a
  checklist toggle (`Ctrl+Enter`), rename, and PDF export (saved next
  to the note) are in the toolbar.
- **History** shows past versions of the current note with diffs.
  Restoring an old version is a deliberate terminal step
  (`icloud-md revert`), never a click.

## Syncing

- **Pull** fetches changes from iCloud (also on startup, and every 5
  minutes while **Auto** is on). Edits made on both sides merge
  automatically when they don't overlap.
- **Push…** always shows a preview first — what will be created,
  updated, moved, or deleted, plus anything that will be refused and
  why. Nothing uploads without your confirmation.
- **Status** shows the same preview on demand; **Sync log** holds the
  full details.
- Small badges in the note list warn you early: brand-new notes,
  unresolved conflicts, notes the push would refuse, and notes changed
  on another device. Saving also warns before writing anything risky,
  and flagged edits are never discarded when you switch notes.
- Deleting a note moves it to Recently Deleted in iCloud (recoverable
  for ~30 days). New folders upload as real Notes folders.

## Your files

Each note is one `.md` file. A small ID block at the top of every file
links it to its iCloud original — don't delete it, or the next push
will treat the note as a brand-new one. Extra notes you add there
(tags, aliases) stay on your machine and never upload. Downloaded
images live in `attachments/` folders next to their notes and are
preview-only: notes with attachments can't be edited back to iCloud.

## Limitations

- Notes with images, audio, or file attachments are read-only upstream;
  you can't add attachments from here either.
- Folders can be created here but not renamed or deleted over there —
  do that in Apple Notes and it syncs down normally.
- Table edits mostly round-trip, but reordering rows/columns is
  refused — the push preview will tell you.
- Sync is on-demand (plus optional auto-fetch), not instant like the
  Mac app.

## If something looks wrong

Open **Sync log** to see exactly what the last operation did. To throw
away local edits on one note and go back to the last synced copy:

```bash
cd ~/Documents/icloud-notes
icloud-md restore "<note file>"
```

## Releasing

Tag a version and push it:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

The release workflow builds the package in an Arch container from
`pkgbuild/PKGBUILD`, signs it and the repository database with the
`PACKAGING_GPG_KEY` secret, and publishes everything as the GitHub release
for that tag, which is what `releases/latest/download` in `install.sh`
resolves to. The workflow refuses to build if the secret's fingerprint is
not the one pinned in `install.sh`.
