# Contacts (icloud-contacts)

Your iCloud Contacts on Omarchy: the address book on your iPhone,
searchable by name, company, email or phone number, with a click to copy a
number or start an email, and a dialog to add or edit a contact.

The iCloud sign-in comes from the
[`icloud-session`](../session/README.md) package, which every iCloud app
on the machine shares: after the first sign-in from any of them,
Contacts opens with no prompt.

## Requirements

- An Apple ID with **Advanced Data Protection turned off** (off by
  default) and **Access iCloud Data on the Web turned on**, the same as
  the other apps. On iPhone/iPad: Settings → your name → iCloud.
- Contacts turned on for iCloud on your devices (Settings → your name →
  iCloud → Contacts).

## Install

On Omarchy (or any Arch Linux), one command trusts the package-signing
key, adds the signed `[icloud-for-omarchy]` repository, and installs the
app:

```bash
curl -fsSL https://github.com/ferdousbhai/icloud-for-omarchy/releases/latest/download/install-contacts.sh | sudo bash
```

`install-contacts.sh` is [`install.sh`](../install.sh) set to install
just this app. Updates arrive with `omarchy update`.

## Using it

- The sidebar lists every contact, sorted by last name, with a search box
  (Ctrl+F) that matches names, nicknames, companies, emails and, typed as
  digits, phone numbers. Enter opens the first match. Your own card says
  **My Card**.
- The card shows phone numbers, emails, links, addresses, the birthday and
  notes. Click a number, address or note to copy it; an email opens your
  mail app and a link your browser.
- **+** (Ctrl+N) adds a contact, the pencil (Ctrl+E) edits one: names,
  company, job title, nickname, birthday, notes, and a row per phone, email
  and URL. Empty a row to remove that value; type in the last row to add
  one, with a label first if you like (`work=+1 555 0100`).
- The trash button deletes a contact, after asking: it goes from all your
  devices, and iCloud keeps no Recently Deleted for contacts (only
  icloud.com's Data Recovery, which restores a whole earlier version of the
  address book, brings it back).
- The window syncs on opening, every 5 minutes while it is on screen, and
  on **Ctrl+R**.
- When nobody is signed in to iCloud, a banner offers **Sign In**.

Edits keep everything this app does not show: a photo, social profiles,
related names and dates go back to iCloud as they were. Changes go straight
to iCloud: without a network a change fails and says so, and nothing is
queued.

### What is kept on this computer

`~/.local/share/icloud-contacts/cache.json`: the contacts as last synced,
and the tokens iCloud's contacts service hands out. It's only a cache, since
iCloud holds the contacts. Delete the directory to start over; the next
sync fetches everything.

## Command line

`icloud-contacts` given a command runs it without the window; with no
command it opens the app, `icloud-contacts-app`.

```console
$ icloud-contacts list                          # everyone, by last name
$ icloud-contacts list acme                     # matching a name, company or email
$ icloud-contacts list 5550100                  # matching a phone number's digits
$ icloud-contacts show "ann lee"
$ icloud-contacts show me
$ icloud-contacts add --first Ann --last Lee --phone work=+1 555 0100 --email ann@acme.com
$ icloud-contacts edit ann --job-title Manager --phone home=+1 555 0177 --remove-email ann@old.com
$ icloud-contacts edit ann --nickname ""        # an empty value removes a field
$ icloud-contacts delete ann --yes
$ icloud-contacts export --output contacts.vcf  # everyone, as vCard 3.0
$ icloud-contacts sync
```

Every command, how contacts and labels are named, the output modes (text
on a terminal, JSON when piped), the exit and error codes and the JSON
shapes are in the [command-line reference](https://github.com/ferdousbhai/icloud-for-omarchy/blob/main/docs/CLI.md).

## Building from source

Needs `rust`, `cargo`, `gtk4` and `libadwaita`. From the repository root:

```bash
bin/build icloud-contacts
target/release/icloud-contacts-app      # the app; icloud-contacts is the command line
```

### Running without an Apple account

`examples/fake_contacts.rs` is an in-memory fake of iCloud's contacts
service (tokens that expire, changeset answers, etags and their 412),
seeded with four contacts, one of them the "me" card. `icloud-session`'s
mock mode points every service at it:

```bash
cargo run -p icloud-contacts --example fake_contacts &     # 127.0.0.1:8765
ICLOUD_SESSION_MOCK=1 XDG_DATA_HOME=/tmp/contacts-dev cargo run -p icloud-contacts --bin icloud-contacts-app
ICLOUD_SESSION_MOCK=1 cargo run -p icloud-contacts -- list --data-dir /tmp/contacts-dev/icloud-contacts
curl -s http://127.0.0.1:8765/fake/requests                    # what was sent
```

### Tests

```bash
cargo test -p icloud-contacts
```

`tests/cli.rs` runs the built command line against the fake for every
command. `tests/model.rs` edits seeded random contacts through both the
command line and the window's dialog (`src/draft.rs`), some over another
device's change, and checks that each edit changes only what it names; it
also reads every vCard it exports back (`MODEL_SEED=<n>` reruns one case).
The library has no GTK dependency (the window's code is in
`src/bin/app/`), so `cargo test --no-default-features` runs on a machine
without the GTK stack.

## How it works

Contacts come from the `contacts` web service, reached as icloud.com's
Contacts reaches it (`src/api.rs`). There is no public description of it;
the request shapes were checked against iCloud itself.

- **Reads**: `GET /co/startup` hands out a `prefToken` and a `syncToken`
  (and the "me" card's id), and `GET /co/contacts?…&limitTo=0` with them
  returns every card. A sync fetches them all and replaces the cache: an
  address book of a few thousand cards is a few hundred kilobytes.
- **Writes**: `POST /co/contacts/card/` with `{"contacts": [card]}`
  creates a card (with a new uppercase UUID as its `contactId`);
  `&method=PUT` replaces one, whole, as of its `etag`; `&method=DELETE`
  deletes one. A card is kept as iCloud sent it (`src/model.rs`), and an
  edit changes a copy of it, so the fields this app does not read go back
  untouched.
- **The answer to a write is a changeset**: every card changed since the
  request's `syncToken`, which can list other people's cards before the
  one written. The card written is found by its `contactId`, never by
  position.
- **Conflicts**: a card that changed on another device since its `etag`
  was read is refused (HTTP 200, `errors: [{errorCode: 412}]`). The app
  then syncs, applies the same edits (set this field, add or remove this
  number) to the fresh card, and writes once more.
- **Tokens** go in the query percent-encoded (they are base64). One iCloud
  no longer accepts is HTTP 420 "Invalid sync token": a write then asks
  `/co/startup` for fresh ones and tries once more.

Not done yet: groups are not shown, photos show as initials, labels of
existing values can't be changed in the window (remove the value and add
it again with a label), and addresses are read-only.

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing). Releases are tagged `v<version>`: one
suite version for every package.
