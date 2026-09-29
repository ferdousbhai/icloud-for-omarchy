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
| `golden` | what icloud-md's own code makes of it (below) |

Note goldens: `text` (decoded visible text), `attributeRunLengths`,
`roundTrips` (`noteDocumentRoundTrips`), `document` (run/replica counts),
`embedSlots`, `format` (`decodeNoteFormat` paragraphs, the
`doc::format::FormatParagraph` JSON shape; lengths in UTF-16 units) and
`markdown` (`renderNoteMarkdown` of that format). Table goldens:
`roundTrips` (`tableDocumentRoundTrips`), `grid`, `markdown`
(`decodeTableMarkdown`); a failing step is `{ "error": "<message>" }`.

Regenerate (must reproduce the files byte for byte):

```bash
ICLOUD_MD=../../../coddingtonbear/icloud-md   # the default
$ICLOUD_MD/node_modules/.bin/tsx tests/differential/export-fixtures.mts
```
