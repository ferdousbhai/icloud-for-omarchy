# Port plan: icloud-md 0.6.2 → icloud-notes-sync (Rust)

Source: /home/dous/github.com/coddingtonbear/icloud-md (MIT, Adam Coddington),
pinned to v0.6.2 behaviour (HEAD 27072f1 differs only in docs and the session
file). The real vault is `layoutVersion: 3`, `generator: "icloud-md 0.6.2"`.
Decided 2026-09-29. All Apple calls go through icloud-session (then ../icloud-session, now session/); icloud-md's
auth/ and session files are dropped.

## 0. What icloud-notes depends on

Runs the CLI in the vault root, stdout+stderr merged (MergedChannels):

| call | Notes reads |
|---|---|
| `clone --account <dsid> --non-interactive <root>` | exit code (reads `titleMode` from state.json afterwards; never passes `--filename-as-title`, but filename-mode vaults must work) |
| `pull`, `push` | exit code; text logged |
| `--json status` | `{entries:[{kind,file,resolution,reason?,remark?,folderTitle?,previousFile?,pendingRename?}],unchanged:int,notices:[{level,message}]}`; kind ∈ create/createFolder/update/delete/move/rename; resolution ∈ ready/refused/conflict/noop |
| `--json history <relpath>` | `{mode:"epochs",epochs:[{id,timestamp,changed[],carriedOver[]}]}` |
| `diff <relpath> <id>` or `<from>..<to>` | text |

Exit codes: 0 ok, 1 known error, 2 usage, 3 = status has entries / diff has
differences, 70 internal. Sign-in detection: output containing `icloud-md
reauthenticate`; also watches `attempting silent re-authentication`.

Latent Notes bugs fixed at switch-over: any non-zero exit = failure (so status/diff
exit 3 look failed); merged channels can corrupt parsed JSON.

Vault files Notes reads: `.icloud-md/` (or legacy `.icloud-notes-sync/`)
`state.json` (`titleMode`, `folders["DefaultFolder-CloudKit"].dirName`,
`notes.*.file`, `notes.*.unpublishableReason`); front matter exactly
`---\napple-note-id: <ID>\n---\n\n`; per-folder `attachments/` linked
`[..](attachments/...)`; diff3 markers `<<<<<<<` `|||||||` `=======` `>>>>>>>`;
file mtimes = note modification dates.

## 1. Inventory

Keep: note codec (noteDocument, noteFormat, formatReconcile, decode/encodeNoteRecord,
noteText, versionedDocument, mergeableDataPool, tables: decodeTableRecord,
tableEdit, tableCellEdit, tablePushEdit, markdownTable; embeds: embedPushEdit,
noteAttachments, unknownContent); Markdown (renderNoteMarkdown, parseNoteMarkdown,
frontmatter, noteIdFrontmatter, noteTitleParagraph, titleFilename, filename);
vault (cloneState, vaultMigrations, baseCopy, versionHistory, noteEpoch,
localFileState, trackedFile, noteIdPairing, pendingRename, folderLayout,
folderTree, folderReconcile, folderCreate, attachmentSync, noteTimestamps,
mergeConflict, pushPlan); cloudkit/databaseClient (parsing and paging; transport
via icloud-session); commands clone, pull, push (every refusal), status, history,
diff, restore; cli subset, output, pullReport, reportStyle, errors, vaultRoot.

Drop: auth/*, session.ts, configDir session use, object/bugReport/revert/delete
commands, bugReport*, scripts/*, integration/webOracle.ts, marketing/,
lastError.ts, debugLog.ts (small log under ~/.local/state/icloud-notes-sync).

## 2. Layout and dependencies

```
proto/ (from icloud-md)  build.rs
src/lib.rs  src/main.rs (clap)
src/cloudkit/{mod,types,client,transport}.rs
src/doc/{proto,document,format,reconcile,text,tables,table_edit,embeds,decode,encode}.rs
src/md/{render,parse,frontmatter,title,filename,table}.rs  src/diff3.rs
src/vault/{state,migrate,base,history,epoch,layout,folders,attachments,pairing,local}.rs
src/cmd/{clone,pull,push,plan,status,history,diff,restore,output,errors}.rs
tests/
```

- Protobuf: `protobuf` 3.x + `protobuf-codegen` (pure). proto2 with field
  presence; unknown fields must survive decode→encode (prost drops them: unsafe).
- flate2, base64, serde/serde_json (preserve_order), clap, thiserror, uuid,
  unicode-normalization, tempfile (dev), icloud-session (path).
- Markdown: `markdown` (markdown-rs) to mdast with GFM + positions. Serializer:
  try `mdast_util_to_markdown`; where it differs, port the needed parts of
  mdast-util-to-markdown + mdast-util-gfm (node types used, `unsafe` escaping,
  `tablePipeAlign:false`), frozen by golden tests.
- diff3: hand-port node-diff3 (MIT): LCS, diffIndices, diff3MergeRegions,
  mergeDiff3, diffComm (Myers-based crates align differently).

Byte-exact: note Markdown, front matter, file/dir names (homoglyphs, collisions,
title mode), state.json (key order, 2-space, trailing newline), base/<ID>.md,
history/<ID>/*.json, epochs/, push protobuf bytes and CloudKit request JSON,
conflict markers, --json outputs. Not: human text (except sign-in marker),
progress, logs.

`generator` written as `"icloud-notes-sync X.Y.Z"`; never bump `layoutVersion`;
refuse vaults with `layoutVersion > 3`.

## 3. Workstreams (each owns its directories; seams stubbed first by D)

A. CloudKit + transport (`src/cloudkit/**`, build.rs, proto/):
```rust
pub struct CloudKitRecord { record_name, record_type, fields: BTreeMap<String, FieldValue>, record_change_tag: Option<String>, deleted: bool, parent_record_name: Option<String>, created: Option<Stamp>, modified: Option<Stamp>, participants: Vec<Participant>, share: Option<String> }
pub trait Transport { fn post_json(&self, path:&str, body:&Value) -> Result<Value, CkError>; fn download(&self, url:&str, dest:&Path) -> Result<u64, CkError>; }
pub struct LiveTransport(icloud_session::Session); // base = webservices().url("ckdatabasews")
pub struct ReplayTransport { cassette: PathBuf, record: bool }
impl Database<T> { fetch_all_note_records(token) -> ZoneChanges; fetch_shared_zone_ids; lookup_records(names, zone); update_records(ops, zone); create_note_record; fetch_asset(url, dest) }
pub enum CkError { SignInRequired, Conflict{..}, Http{status,body}, Other(String) }
```
B. Note codec (`src/doc/**`):
```rust
pub fn classify_note_record(r:&CloudKitRecord, o:&ClassifyOptions) -> NoteDecodeResult;
pub fn parse_note_document(raw:&[u8]) -> Result<NoteDocument>; pub fn encode_note_document(&NoteDocument) -> Vec<u8>;
pub fn apply_text_edit(doc:&mut NoteDocument, new_text:&str, o:&ApplyTextEditOptions) -> bool;
pub fn build_note_update_fields(..) / build_note_create_fields(..) -> BTreeMap<String, UpdateFieldValue>;
```
`FormatParagraph`, `InlineSpan`, `ParagraphKind` in doc/format.rs are the contract with C.

C. Markdown, naming, diff3 (`src/md/**`, `src/diff3.rs`):
```rust
pub fn render_note_markdown(p:&[FormatParagraph]) -> String;
pub fn parse_note_markdown(md:&str) -> Result<Vec<FormatParagraph>, ParseRefusal>;
pub fn split_frontmatter(text:&str) -> Envelope; pub fn with_note_id(body:&str, id:&str) -> String;
pub fn note_filename(title:&str, taken:&HashSet<String>) -> String;
pub fn merge_diff3(base:&str, local:&str, remote:&str) -> MergeOutcome; pub fn diff_comm(a:&[&str], b:&[&str]) -> Vec<CommHunk>;
```
D. Vault, commands, CLI (`src/vault/**`, `src/cmd/**`, `src/main.rs`, `tests/cli_*`):
```rust
pub struct CloneState { account, sync_token, shared_zone_sync_tokens, replica_id, title_mode, notes: IndexMap<String, NoteEntry>, folders, sharer_homes, attachments, table_attachments, trashed, layout_version, generator }
pub fn read_clone_state(dir) -> Result<Option<CloneState>>; pub fn write_clone_state(dir, &CloneState) -> Result<()>; // atomic
pub fn run_clone/run_pull/run_push/run_status/run_history/run_diff(..)
```
Every push refusal (push.ts, pushPlan.ts; ~31 sites) becomes an enumerated
`Refusal` with verbatim reason strings; a test lists each site.

## 4. Tests

1. Port unit tests + fixtures (realFixtures.ts → tests/fixtures/) for every kept
   module, databaseClient.*.test.ts, and commands {clone, pull, pullResync,
   pullTitleMode, push, pushTitleMode, status, history, diff, restore}, cli/output.
2. Differential harness (tests/differential/): Node `driver.ts` via tsx importing
   the unmodified icloud-md clone, HOME in a temp dir with a fake session file,
   `globalThis.fetch` stubbed to serve /validate and ckdatabasews from a cassette
   and record request bodies; Rust side via ReplayTransport on the same cassette.
   Compare `diff -r` of vaults (normalize generator, injected clock), --json
   outputs, exit codes, push request bodies (JSON- and protobuf-bytes-equal).
   Cassettes: recorded read-only live sessions, then hand mutations for push.
   Also pull/status/push --dry-run on a `cp -a` copy of the real vault.
3. Live: read-only clone of the real account into two scratch dirs (icloud-md via
   the mirror; icloud-notes-sync), `diff -r`; same for pull after a phone edit.
   Push only inside a dedicated folder `icloud-notes-sync-itest` following
   icloud-md's integration/README.md (deletes need both: inside the folder AND
   created this run or `(itest-<runId>)` prefix; refuse to start if the folder
   holds unprefixed notes; never create folders). After each push, a fresh
   icloud-md clone is the oracle.

## 5. Switch-over in icloud-notes

Same verbs/flags; `--account` checked against `Session::dsid()`;
`--non-interactive` accepted, no-op. notesbackend.cpp: program
`icloud-notes-sync`, rename messages/log prefix; stop MergedChannels (stdout =
JSON, stderr = log); exit 3 = success for preview and diff; sign-in via new exit
code 4, keep matching `icloud-md reauthenticate` during transition.
backgroundsync.cpp: drop the nvm/volta/npm PATH search. PKGBUILD: depends
icloud-notes-sync, drop nodejs; own PKGBUILD for the new crate. README: rename
restore/revert mentions. Later: remove the ~/.config/icloud-md mirror and its
inotify adoption from icloud-sessiond.

## 6. Risks and order

Risks: remark-stringify escaping (golden corpora from the Node driver over every
real note); unknown-field preservation (protobuf crate + bytes-equal round trips);
node-diff3 quirks; keeping every refusal (enumerated + checklist); CRDT
clock/replica stamping; drift after 0.6.2 (pin); CloudKit "session expired"
must map to CkError::SignInRequired → exit 4.

Order: (1) D seam stubs + CLI skeleton, A proto codegen, Node driver;
(2) A, B, C parallel with D's vault state/migrations; read path (clone, pull)
first, differentially tested; (3) history, diff, restore; (4) status and push
--dry-run with request-body diffs; (5) live push in the test folder;
(6) Notes switch-over, then remove the icloud-session mirror.

## 7. Deliberate differences from icloud-md 0.6.2 (after parity)

Upstream PR #29 (ferdousbhai, "Fetch the text of notes too large to store it
inline", open and unmerged as of 2026-09-29; fork branch
`fetch-asset-note-bodies`, commits 768bcb3 and 10fa221 in the icloud-md clone)
is not in 0.6.2, so 0.6.2 skips notes whose body lives in TextDataAsset (the
user has one: an ~833 KB pinned note). Port it once the read path matches 0.6.2:
request TextDataAsset, inline the downloaded bytes, mark such notes unpublishable
("is so large that Apple keeps its text in a separate file, which can't be
written back yet"). Differential tests then run with a flag that turns it off.

Done: `cloudkit::client` (`note_desired_keys`, `Database::inline_asset_bodies`
at the end of every zone walk and after the shared lookup backfill; a failed
download fails the fetch) and `doc::decode::classify_note_record` (the PR's
reason string). `ICLOUD_NOTES_SYNC_ASSET_BODIES=0` restores stock 0.6.2; the
0.6.2 differential scenarios run with it, the `asset-*` scenarios compare with
the fork branch (see tests/differential/README.md). PR tests ported in
tests/cloudkit_asset_body.rs. Live: a fresh clone of the real account matches
the installed PR #29 build byte for byte (bar `generator`).

### Records listed twice by CloudKit (not in 0.6.2)

Live testing (2026-09-29, a ~118-note account) found about 1 clone in 9
writing one note twice: a second byte-identical file under a uniquified
name (`X 2.md`), only one of the two tracked in `state.json`, so the next
`push` would create the other as a duplicate note. icloud-md 0.6.2 does exactly the same. Cause: a `changes/zone` walk
occasionally lists the same record on two pages (the run logs show the fetch
total one higher than the clones before and after it with no note created in
between, and `written` one higher, for icloud-md and the port alike);
`fetchZoneNoteRecords` concatenates pages and `clone` materializes every
occurrence (and `state.json` keeps only the last one, so the first file is
untracked). The port deviates in two places:

- `cloudkit::client::dedupe_zone_records`, applied at the end of every zone
  walk (`walk_zone`, so private and each shared zone, before the
  `records/lookup` backfill and asset inlining; and `fetch_all_zone_records`):
  one record per recordName within a zone. The winner is the occurrence with
  the greater `modified.timestamp` when both have one and they differ, else
  the later occurrence in listing order (a later page is a later server read,
  never staler; this also lets a later tombstone replace a live copy).
  `recordChangeTag`s are opaque and never compared. The winner keeps the
  position of the first occurrence. The same recordName in a different zone
  (private vs shared) is a different record and is kept.
  `fetch_shared_zone_ids` likewise keeps a zone listed twice once. `on_page`
  progress still counts raw page sizes. Pull uses the same fetches, so it is
  covered too.
- `push` (and `status`) refuse to create an untracked file whose
  `apple-note-id` names a note this clone tracks while that note's own file is
  still present (`Refusal::CreateDuplicatesTrackedNote`: "carries the
  "apple-note-id" of <tracked file>, a note this clone already tracks, so
  pushing it would create a duplicate of that note - delete this file if it
  is a leftover copy, or remove its "apple-note-id" line to push it as a new
  note"). 0.6.2 treats such a file as a copy and creates it. Moves (the
  tracked file gone, one claimant) and ambiguous claims are unchanged.

Tests: tests/cloudkit_dedupe.rs (same page, across pages, timestamps vs page
order, differing tags, tombstone, private vs shared plus the lookup
backfill, a shared zone listed twice), tests/cmd_push.rs
(`copy_with_original_in_place_is_refused_as_a_duplicate`,
`byte_identical_twin_from_a_double_clone_is_refused`), tests/cmd_refusals.rs
(port-only row), and the differential scenario `dup-clone`
(`portDeviation: dedupe-records`): icloud-md's expectation is recorded as
usual and asserted to contain the duplicate; the port is asserted to send the
same requests and to produce icloud-md's `tiny-clone` vault/stdout/mtimes.

### New notes listed without their text (not in 0.6.2)

Found in review (2026-09-29). In 0.6.2's `pull` (`src/commands/pull.ts`
L294-306) a note new to the vault that the private `changes/zone` walk
delivers without `TextDataEncrypted` is counted in `skippedNewUnsyncable`
and skipped, while the new `syncToken` is still saved - so it is never
added until it changes again. (Shared zones already look such notes up and
hold back the whole zone, carrying its old token.) The port deviates in
`cmd::pull::backfill_new_note_bodies`, run after both fetches:

- Every live Note in the private listing that has no string
  `TextDataEncrypted` and is not tracked is looked up by id (one private
  `records/lookup`, merged in with `merge_looked_up_records`, then
  `inline_asset_bodies` when asset bodies are on - the shared backfill's
  steps). A note the lookup fills is added like any other.
- If any still has no text, it is skipped and counted as before, a warning
  says so ("Skipped N new note(s) that came through without their text, even
  when looked up - this vault's sync token was kept where it was, so the next
  pull will look for them again"), and `state.json` keeps the previous
  private `syncToken`. Chosen over remembering the record names in state:
  no new state field, and the next walk simply delivers the note again. The
  cost is that everything else in that window is delivered again too and
  re-applied (a clean note is rewritten with the same text and reported as
  updated), until the text arrives or the note is deleted.
- Tracked notes listed without text keep 0.6.2's behaviour (a warning, the
  file left alone).

`clone` has the same gap in 0.6.2 (`src/commands/clone.ts`): such a note is
counted in `skippedUndecodable` and the listing's `syncToken` is saved past
it, so a later `pull` does not see it unless it changes. Since 0.1.2 the port
closes it the same way, with the same helper (nothing is tracked yet, so
every live private Note without text is looked up):

- A note the lookup fills is written like any other.
- If any still has no text, it is skipped and counted in
  `skippedUndecodable` as before, a warning says so ("Skipped N note(s) that
  came through without their text, even when looked up - no sync token was
  saved for your own notes, so the first pull will look for them again (and
  re-read the rest)"), and `state.json` gets no `syncToken` - there is no
  previous one to keep. The first pull then walks the private zone from
  scratch, as clone did: every note is delivered again and re-applied (the
  cost the pull case accepts), and the note goes through pull's lookup
  above.
- Shared zones already behaved: the fetch looks such notes up, and a zone
  still missing text is skipped with its own warning and no token, so the
  first pull fetches it from scratch.

Tests: tests/cli_differential.rs `bodyless_pull_*` with the scenarios
`bodyless-pull` and `bodyless-pull-unfilled` (`portDeviation:
look-up-new-bodyless-notes`): icloud-md's expectation is asserted to skip
the note, send no lookup and save the new token; the port is asserted to
send the same requests plus one lookup, and then either to add the note
(lookup has the text) or to keep the previous token with otherwise
icloud-md's summary and vault (lookup still without text).
For clone, `bodyless_clone_*` with the scenarios `bodyless-clone` and
`bodyless-clone-unfilled` (same `portDeviation`) assert the same way: the
note written and the token saved, or no `syncToken` with otherwise
icloud-md's summary and vault. tests/cmd_clone.rs covers the clone followed
by a pull that walks from scratch and adds the note, and a shared zone still
without text.
