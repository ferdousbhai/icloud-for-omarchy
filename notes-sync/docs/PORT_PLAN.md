# Port plan: icloud-md 0.6.2 → icloud-notes-sync (Rust)

Source: /home/dous/github.com/coddingtonbear/icloud-md (MIT, Adam Coddington),
pinned to v0.6.2 behaviour (HEAD 27072f1 differs only in docs and the session
file). The real vault is `layoutVersion: 3`, `generator: "icloud-md 0.6.2"`.
Decided 2026-09-29. All Apple calls go through ../icloud-session; icloud-md's
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
