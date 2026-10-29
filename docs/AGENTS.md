# Driving the iCloud apps as an agent

A compact reference for an AI agent (or a script) using iCloud Notes,
Photos, and Find My on this computer through their command lines. The full
audit, every JSON shape and every error code are in [CLI.md](CLI.md).

## The rules that always hold

- **Output.** Piped (as an agent's commands are), stdout is a JSON envelope:
  `{"ok":true,"data":…,"summary":"…"}`. Pass `--quiet` to get only `data`,
  the bare result, which is what the examples below print; or read `.data`
  from the envelope. `--ids-only` prints one id per line, `--count` a
  number. Flags combine (ids-only > count > quiet > json > styled) and go
  before or after the command. Progress and warnings go to stderr.
- **Errors.** In those modes a failure is one JSON document on stdout
  (pretty-printed over several lines: parse all of stdout), stderr empty:
  `{"ok":false,"error":{"code":"…","message":"…","hint":"…"}}` (`hint` is
  optional). Branch on the exit status and `error.code`; show `message` and
  `hint` to the user (`hint` names the command that fixes it).
- **Exit codes**, the same in every tool:

  | Exit | Meaning | What to do |
  |---|---|---|
  | 0 | ok | |
  | 1 | other failure | read `error.code` |
  | 2 | usage: bad arguments, an ambiguous name, it already exists, or a destructive command without `--yes` | fix the call |
  | 3 | sign-in needed or refused, or not set up | `sign_in_required`/`session_rejected`: ask the user to run `icloud-session sign-in`; `find_my_auth_required`: `icloud-session authorize-find-my`; `photos_unavailable`/`find_my_unavailable`/`notes_unavailable`: the service is off for this Apple ID; `not_cloned`: `icloud-notes clone` |
  | 4 | not found | check the name or id |
  | 5 | a service unavailable: no network (`offline`, `network`), icloud-sessiond unreachable (`session_service`), keyring locked, iCloud failing | retry later, or tell the user what `hint` says |

- **`ok:true` with a non-zero exit** means the command produced its result
  but not everything succeeded: a batch where some items failed (each has
  its own `error`), or an `icloud-notes` sync/pull/push/clone that ran and
  failed (`data.error` says why; `data.sync.error` for a change's `--push`).
  `status` (icloud-session, icloud-photos, icloud-notes) prints the status
  and exits 3 when signed out. `push --dry-run` and `diff` exit 0 whether or
  not there are changes.
- Nothing prompts without a terminal. Destructive commands refuse (exit 2,
  `usage`) unless given `--yes`: `icloud-notes delete | delete-folder |
  restore`, `icloud-photos delete`, `icloud-findmy play-sound | lost-mode`.
  Pass `--yes` only when the user asked for that exact action.
- `TOOL COMMAND --help` describes any command and its `data`.

## Auth: icloud-session owns the sign-in

One Apple account per computer, held by the `icloud-session` daemon; every
tool takes its session from it and none signs in by itself.

```console
$ icloud-session --quiet status
{
  "signed_in": true,
  "apple_id": "you@example.com",
  "full_name": "Your Name",
  "dsid": "1234567890",
  "expires_at": 1793000000,
  "signing_in": false,
  "find_my_authorized": true,
  "find_my_password_stored": false,
  "sign_out_reason": null
}
```

(Signed out it prints the same object with `signed_in: false` and exits 3;
exit 5 `session_service` means the daemon itself is unreachable.)

Needs a human, always:

- **Signing in**: `icloud-session sign-in` opens Apple's own page in a
  window; the person types the password and the two-factor code there
  (ticking "Keep me signed in", and "Trust", makes it last about 30 days).
  The command waits for the window and exits 0 once signed in, 3
  (`sign_in_not_completed`) if it was closed without. `--no-wait` returns at
  once; poll `icloud-session --quiet status` until `signing_in` is false.
- **Find My's password page** (exit 3, `find_my_auth_required`, from
  `icloud-findmy`): `icloud-session authorize-find-my`, the same way. Once
  the user has run `icloud-session set-password` (it reads the password
  from the terminal, or from stdin: `bw get password "Apple ID" |
  icloud-session set-password`, `op read "op://Private/Apple ID/password" |
  icloud-session set-password`, and stores it in the keyring; Apple refusing
  it is `password_rejected`, exit 3), the daemon answers that page itself.

`expires_at` (Unix seconds) says when the sign-in lapses; warn the user a
few days ahead.

## Notes: `icloud-notes`

Notes are Markdown files in `~/Documents/Notes`, one folder per
Notes folder, synced with iCloud. Always go through `icloud-notes`, never
edit the files or run the sync engine (`icloud-notes-sync`, inside the
icloud-notes package, off PATH) yourself: `icloud-notes` applies the app's
rules, takes the vault's lock, and reaches every engine command you need.

- **NOTE** is a path in the vault (`Notes/Groceries.md`, `.md` optional),
  an apple-note-id, or a title exactly one note has (`ambiguous`, exit 2,
  otherwise: use the path).
- **The lock.** Commands that change the vault or sync take the lock the
  Notes window holds while it is open. If Notes is open they fail at once
  with `vault_busy` (exit 1): tell the user to make the change in Notes or
  quit it. A background sync holding it is waited for (30 s; `--wait SECS`).
  Reading (`status`, `folders`, `list`, `read`, `search`, and `history`,
  `diff`, `push --dry-run`) never needs it.
- **Changes stay local until a sync.** Add `--push` to a change to sync
  right after it (push, then pull), or run `icloud-notes sync`. Before a
  sync you did not make yourself, preview with `icloud-notes push --dry-run`
  (exit 0; `entries` lists what would go, `entries[].resolution:
  "refused"` what will not and why). A sync without a network fails with
  `offline` (or `network`), exit 5: retry once it is back.
- A deleted note goes to the trash and, on push, to Recently Deleted in
  iCloud (recoverable there for about 30 days).
- Read-only notes (`read_only` set: very large notes, attachments Apple
  keeps elsewhere) cannot be written, renamed or moved: say so.

```console
$ icloud-notes --quiet list --folder Notes
[{"path":"Notes/Groceries.md","folder":"Notes","file":"Groceries.md","title":"Groceries","snippet":"bread",
  "modified":"2026-09-30T08:12:00Z","modified_ms":1790755920000,"id":"8C1F...","flags":[]}]
$ icloud-notes --quiet read Groceries
{"path":"Notes/Groceries.md","title":"Groceries","id":"8C1F...","body":"# Groceries\nbread\n","read_only":null,
 "attachments":[],"conflicts":[],"conflicts_unreadable":false,"flags":[],...}
$ icloud-notes --quiet new "Packing" --body "- [ ] passport" --push
{"action":"new","path":"Notes/Packing.md","folder":"Notes","file":"Packing.md","title":"Packing",
 "sync":{"ok":true,"runs":[{"command":"push","ok":true,"exit_code":0,"result":{...}},{"command":"pull",...}],"log":"..."}}
$ icloud-notes --ids-only list --flag conflict
Work/Plan.md
```

(Shown on fewer lines than the tools print them.)

| Command | Does |
|---|---|
| `status` | vault, sign-in, lock holder, notes flagged `conflict`/`new`/`read-only`/... (exit 3 signed out) |
| `folders`, `list [--folder F] [--flag FLAG]`, `read NOTE [--raw]`, `search QUERY` | read |
| `new TITLE [--folder F] [--body T \| --file P \| --stdin]` | create (default folder: the account's "Notes"; the body goes below the title line) |
| `write NOTE (--body T \| --file P \| --stdin) [--append] [--force]` | replace the text `read` shows (its first line is the title), or append to it |
| `rename NOTE TITLE`, `move NOTE FOLDER`, `delete NOTE --yes`, `toggle NOTE LINE` | as in the window (LINE counts from 1 in `read`'s body) |
| `resolve NOTE --all local\|remote\|both` / `--choices C1,C2,...` | settle conflict blocks (`local` = this computer, `remote` = iCloud) |
| `recover NOTE --strip \| --synced` | unreadable conflict markers (`conflicts_unreadable`): keep every line, or the last synced text; backs up first |
| `new-folder NAME [--in F]`, `rename-folder F NAME`, `delete-folder F --yes` | folders |
| `sync`, `pull`, `push [--dry-run]`, `clone` | talk to iCloud (exit 3: sign in (`sign_in_required`, `session_rejected`), or `not_cloned`; an engine error such as `account_mismatch` or `bad_response` keeps its own code and exit) |
| `history NOTE [--records]`, `diff NOTE REF`, `restore NOTE --yes`, `export-pdf NOTE` | versions, discard local edits, PDF next to the note |

`write` refuses (`guardrail`) text that adds conflict markers or another
note's id; `--force` overrides, but prefer `resolve`. Conflicts: `read`
lists each block's `local` and `remote` lines; ask the user which to keep
when it is not obvious.

## Photos: `icloud-photos`

A local catalog of the iCloud library; `sync` refreshes it.

```console
$ icloud-photos --quiet albums
[{"id":"A1B2...","name":"Holiday","count":42}]
$ icloud-photos --quiet list --album A1B2... --limit 1
[{"id":"AQx...","filename":"IMG_0001.HEIC","created":"2026-08-01T10:00:00Z","created_unix":1785578400,"kind":"live",
  "size":2048576,"width":4032,"height":3024,"local_path":null,"live_path":null}]
$ icloud-photos --quiet download AQx... --out ~/Downloads/holiday
[{"id":"AQx...","path":"/home/me/Downloads/holiday/2026/08/IMG_0001.HEIC","live_path":"...MOV","live_error":null}]
```

| Command | Does |
|---|---|
| `status` (exit 3 signed out), `sync [--full]` | state; refresh the catalog |
| `albums`, `list [--album ID] [--since DATE] [--kind photo\|video\|live] [--limit N]`, `info ID` | read the catalog |
| `thumb ID [--out P]`, `download ID... \| --all [--medium] [--out DIR]`, `open ID` | fetch (originals go to `~/Pictures/<year>/<month>/` unless `--out`) |
| `upload FILE... [--album ID] [--no-sync]` | upload, then (unless `--no-sync`) sync until the new ids are in the catalog |
| `delete ID... --yes` | to Recently Deleted on every device |
| `restore ID...` | back from Recently Deleted (undoes `delete`) |
| `config [--library-dir DIR] [--download on-demand\|all]`, `prune-cache` | preferences, cache |

`download`, `delete`, `restore` and `upload` exit 1 when some items failed, with the
full result: each failed item carries its own `error`. `--ids-only` lists
the ids that succeeded. An id not in the catalog is `not_found` (exit 4):
`sync` first.

## Find My: `icloud-findmy`

```console
$ icloud-findmy --quiet devices
[{"id":"aVBo...","name":"Dous's iPhone","model":"iPhone 15 Pro","class":"iphone","battery_percent":81,
  "charging":false,"online":true,"lost_mode":false,"can_play_sound":true,"can_lost_mode":true,
  "last_fix":{"time":"2026-09-30T09:58:02Z","timestamp_ms":1790762282000,"age_secs":95,"accuracy_m":12.0,"is_old":false}}]
$ icloud-findmy --quiet locate "iphone" --wait 30
{"id":"aVBo...","name":"Dous's iPhone",...,"last_fix":{...,"lat":60.17,"lon":24.94}}
$ icloud-findmy --quiet play-sound "iphone" --yes
{"action":"play_sound","device":{"id":"aVBo...","name":"Dous's iPhone"}}
```

| Command | Does |
|---|---|
| `devices [--locate] [--coords]` | every device; `--locate` wakes them for fresh fixes; coordinates only with `--coords` |
| `locate NAME\|ID [--wait SECS]` | one fresh fix (`no_fix`, exit 1, if none in time) |
| `play-sound NAME\|ID --yes`, `lost-mode NAME\|ID --phone P --message M --yes` | act on a device |
| `history NAME\|ID [--since 24h]`, `prune-history` | the stored trail |

NAME matches case-insensitively, a unique part is enough (`ambiguous`,
exit 2, lists the candidates in `hint`: use the ID; no match is
`not_found`, exit 4). Locations are personal: print coordinates only when
the user asked for them. Lost Mode locks the device; never turn it on
without an explicit request.

## Contacts and tasks

The standalone Contacts and Reminders apps are retired. Use Cloud Contacts and
Cloud Tasks and their skills from ferdousbhai/ghost-skills. They own the iCloud
backends and preserve the legacy caches/notification history. Do not install or
invoke the retired standalone CLIs.

## Recipes

**Sync notes and read one**

```bash
icloud-notes --quiet sync                      # exit 3: ask the user to sign in; 5: offline, retry later
icloud-notes --ids-only search "dentist"       # the paths of matching notes
icloud-notes --quiet read "Notes/Dentist.md" | jq -r .body
```

**Add a note and push it**

```bash
icloud-notes --quiet new "Meeting 30 Sep" --folder Work --body "- agenda" --push
# exit 1, error.code "vault_busy": Notes is open; ask the user to quit it or add it there
# exit 3 with data: written locally, not synced (.sync.error); after the user signs in: icloud-notes sync
```

**Append to a checklist and tick an item**

```bash
icloud-notes --quiet write Groceries --append --body "- [ ] jam"
icloud-notes --quiet read Groceries | jq -r .body   # find the line number (first line is 1)
icloud-notes --quiet toggle Groceries 3 --push
```

**Resolve a conflict**

```bash
icloud-notes --ids-only list --flag conflict
icloud-notes --quiet read "Work/Plan.md"       # .conflicts[i].local / .remote
icloud-notes --quiet resolve "Work/Plan.md" --choices local,both --push
# .conflicts_unreadable true instead: icloud-notes recover "Work/Plan.md" --strip
```

**Download the photos of an album**

```bash
icloud-photos --quiet sync
icloud-photos --quiet albums                   # pick the album's id
icloud-photos --ids-only list --album "$ID" | xargs -r icloud-photos --quiet download --out ~/Downloads/album
```

**Locate a device and play a sound**

```bash
icloud-findmy --quiet locate "iPhone" --wait 30     # exit 3 find_my_auth_required: ask for icloud-session authorize-find-my
icloud-findmy --quiet play-sound "iPhone" --yes
```

## Testing without the real account

Photos and Find My support `ICLOUD_SESSION_MOCK=1` with
`ICLOUD_SESSION_MOCK_URL` and fake Apple servers (`examples/` in each crate);
see their READMEs. Use a temporary `--data-dir` and a scratch Notes vault
(`--vault DIR` or `ICLOUD_NOTES_VAULT`). Contacts/Reminders provider tests
moved to the unified product repos; do not restore their old workspace
packages just to run those tests.
