# Test fixtures

`real/` holds icloud-md v0.6.2's `src/notes/realFixtures.ts` (real
`TextDataEncrypted` and `MergeableDataEncrypted` payloads captured from
iCloud) as JSON, one file per export, listed in `real/index.json`:

| field | contents |
|---|---|
| `name`, `kind` | the TS export name; `note`, `table`, or `tableRevisions` |
| `description` | the TS comments above the export |
| `base64` | the payload exactly as the CloudKit field value (compressed) |
| `revisions` | for `tableRevisions`: the TS array, each entry plus `golden` |
| `golden` | what this crate makes of it (below) |

Note goldens: `text` (decoded visible text), `attributeRunLengths`,
`roundTrips` (`noteDocumentRoundTrips`), `document` (run/replica counts),
`embedSlots`, `format` (`decodeNoteFormat` paragraphs, the
`doc::format::FormatParagraph` JSON shape; lengths in UTF-16 units) and
`markdown` (`renderNoteMarkdown` of that format). Table goldens:
`roundTrips` (`tableDocumentRoundTrips`), `grid`, `markdown`
(`decodeTableMarkdown`); a failing step is `{ "error": "<message>" }`.

The payloads are frozen. The goldens were first exported from icloud-md's
own code and are now recorded from this crate: `doc_fixtures`'
`fixture_goldens_are_current` checks them, and rewrites the ones that differ
with

```bash
ICLOUD_NOTES_SYNC_REGEN=1 cargo test -p icloud-notes-sync --test doc_fixtures
```

`parsed_markdown.json` holds `parse_note_markdown` of the markdown the
reconcile tests (`tests/doc_reconcile.rs`) start from, recorded so those
tests don't move with the parser; `recorded_parses_match_the_parser` checks
and re-records it the same way.
