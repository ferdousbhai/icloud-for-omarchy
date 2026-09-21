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
[`install.sh`](install.sh) in this repo, and the copy the one-liner runs
is the one attached to the latest release, verified with it; read it
first if you like. It also installs an Omarchy `pre-refresh-pacman` hook
so `omarchy refresh pacman` keeps the repository.

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
**Clone my notes**. A real Apple sign-in window opens once; your password
and 2FA stay on Apple's own pages, and the device stays signed in
afterwards. All your notes download into `~/Documents/icloud-notes`,
one Markdown file per note with the title as its first line, just like
in Notes. If the vault is ever missing while the device is still signed
in (a reinstall, say), the app downloads it again on its own, without
asking.

## Everyday use

- **Browse**: folders on the left, notes in the middle (newest first,
  with title, preview line, and date). The search box above the list
  searches every note; picking a result jumps to it. Right-click a folder
  to rename or delete it. Window and pane sizes are remembered.
- **Edit**: edits save on their own once you pause typing, and when you
  switch notes; `Ctrl+S` forces a save when a guardrail has flagged the
  edit. `Ctrl+N` starts a note. Click the title to rename it. Headings,
  emphasis, links and checklists are styled as you type, and stay plain
  Markdown on disk. Bold/italic/link buttons (`Ctrl+B`/`Ctrl+I`/`Ctrl+K`),
  a checklist toggle (`Ctrl+Enter`), and PDF export (saved next to the
  note) are in the toolbar. The window follows the active Omarchy theme.
- **History** shows past versions of the current note with diffs.
  Restoring an old version is a deliberate terminal step
  (`icloud-md revert`), never a click.

## Syncing

Sync is automatic, like Notes, while **Auto** is on (it is, unless you
turn it off):

- Changes from iCloud are pulled on launch and every 5 minutes. Edits
  made on both sides merge automatically when they don't overlap.
- Your edits are pushed about 20 seconds after you stop making them, so a
  burst of typing becomes one push. Nothing waits for a click.
- What keeps this safe is icloud-md itself: a note it cannot push safely
  (attachments, a reordered table, an unresolved conflict) is refused,
  not mangled, and a deleted note moves to Recently Deleted in iCloud
  (recoverable for ~30 days). Small badges in the note list warn you:
  brand-new notes, unresolved conflicts, notes the push refused, and notes
  changed on another device.
- **Push…** shows a preview on demand — what would be created, updated,
  moved, or deleted, plus anything refused and why — and pushes on
  confirmation. **Pull** fetches now. **Sync log** holds the details.
- With **Auto** off, nothing moves until you press Pull or Push….
- New folders upload as real Notes folders.

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
- Folders carry no id in iCloud, so a folder rename here becomes a new
  folder plus note moves on push, and a folder delete moves its notes to
  Recently Deleted; the old folder stays in Notes, empty, until you delete
  it there. Both are in the folder's right-click menu.
- Table edits mostly round-trip, but reordering rows/columns is
  refused — the push preview will tell you.
- Sync runs every 5 minutes and shortly after edits, not instantly like
  the Mac app.

## If something looks wrong

Open **Sync log** to see exactly what the last operation did. To throw
away local edits on one note and go back to the last synced copy:

```bash
cd ~/Documents/icloud-notes
icloud-md restore "<note file>"
```

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
Ghost installer (summonghost.com/install), and both repositories pin its
hash in their tests: change it in both places, and both hashes, together.

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
