// Exports icloud-md's src/notes/realFixtures.ts to tests/fixtures/real/*.json,
// each payload with goldens computed by icloud-md's own (unmodified) code, so
// the Rust codec and renderer can be tested against both the bytes and what
// icloud-md makes of them.
//
//   ICLOUD_MD=../../../coddingtonbear/icloud-md \
//     $ICLOUD_MD/node_modules/.bin/tsx tests/differential/export-fixtures.mts
//
// Re-running it must reproduce the committed files byte for byte.

import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");
const icloudMd = path.resolve(process.env.ICLOUD_MD ?? path.join(repoRoot, "../../../coddingtonbear/icloud-md"));
const outDir = path.join(repoRoot, "tests/fixtures/real");

const mod = async (rel: string): Promise<any> => import(pathToFileURL(path.join(icloudMd, "src", rel)).href);

const fixtures = await mod("notes/realFixtures.ts");
const { decodeNoteString, decompressNoteDocument } = await mod("notes/noteText.ts");
const { noteDocumentRoundTrips, parseNoteDocument } = await mod("notes/noteDocument.ts");
const { decodeNoteFormat } = await mod("notes/noteFormat.ts");
const { renderNoteMarkdown } = await mod("notes/renderNoteMarkdown.ts");
const { decodeNoteEmbedSlots } = await mod("notes/noteAttachments.ts");
const { decodeTableMarkdown, parseTableDocument, gridFromTableDocument, tableDocumentRoundTrips } = await mod(
  "notes/decodeTableRecord.ts",
);

/** The comments directly above `export const NAME`: its `//` lines, and the
 * JSDoc block above those (which may introduce a whole group). */
function descriptions(source: string): Map<string, string> {
  const out = new Map<string, string>();
  const lines = source.split("\n");
  lines.forEach((line, index) => {
    const name = /^export const (\w+)/.exec(line)?.[1];
    if (name === undefined) {
      return;
    }
    let at = index - 1;
    const slashes: string[] = [];
    while (at >= 0 && lines[at]!.startsWith("//")) {
      slashes.unshift(lines[at]!.replace(/^\/\/ ?/, ""));
      at -= 1;
    }
    while (at >= 0 && lines[at]!.trim() === "") {
      at -= 1;
    }
    const block: string[] = [];
    if (at >= 0 && lines[at]!.trim() === "*/") {
      at -= 1;
      while (at >= 0 && !lines[at]!.trim().startsWith("/**")) {
        block.unshift(lines[at]!.replace(/^\s*\* ?/, ""));
        at -= 1;
      }
    }
    out.set(name, [block.join("\n").trim(), slashes.join("\n").trim()].filter((part) => part !== "").join("\n\n"));
  });
  return out;
}

function attempt<T>(fn: () => T): T | { error: string } {
  try {
    return fn();
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  }
}

function noteGolden(base64: string): unknown {
  const compressed = Buffer.from(base64, "base64");
  const raw = new Uint8Array(decompressNoteDocument(compressed));
  const str = decodeNoteString(compressed);
  const format = decodeNoteFormat(str.string, str.attributeRun);
  const doc = attempt(() => parseNoteDocument(raw));
  return {
    text: str.string,
    attributeRunLengths: str.attributeRun.map((run: { length: number }) => run.length),
    roundTrips: noteDocumentRoundTrips(raw),
    document:
      "error" in (doc as object)
        ? doc
        : {
            runs: (doc as any).runs.length,
            replicas: (doc as any).replicas.length,
            minimumSupportedVersion: (doc as any).minimumSupportedVersion,
          },
    embedSlots: decodeNoteEmbedSlots(compressed) ?? null,
    format: format.status === "ok" ? format.paragraphs : { unsupported: format.reason },
    markdown: format.status === "ok" ? renderNoteMarkdown(format.paragraphs) : null,
  };
}

function tableGolden(base64: string): unknown {
  const compressed = Buffer.from(base64, "base64");
  return {
    roundTrips: tableDocumentRoundTrips(compressed),
    grid: attempt(() => gridFromTableDocument(parseTableDocument(compressed))),
    markdown: attempt(() => decodeTableMarkdown(compressed)),
  };
}

const source = await readFile(path.join(icloudMd, "src/notes/realFixtures.ts"), "utf-8");
const docs = descriptions(source);
await mkdir(outDir, { recursive: true });

const index: { name: string; kind: string; file: string }[] = [];
for (const [name, value] of Object.entries(fixtures)) {
  const kind = name.startsWith("TABLE_") ? (typeof value === "string" ? "table" : "tableRevisions") : "note";
  let body: Record<string, unknown>;
  if (typeof value === "string") {
    body = { base64: value, golden: kind === "note" ? noteGolden(value) : tableGolden(value) };
  } else {
    body = {
      revisions: (value as { base64: string }[]).map((revision) => ({
        ...revision,
        golden: tableGolden(revision.base64),
      })),
    };
  }
  const file = `${name.toLowerCase()}.json`;
  const json = { name, kind, source: "icloud-md v0.6.2 src/notes/realFixtures.ts", description: docs.get(name) ?? "", ...body };
  await writeFile(path.join(outDir, file), JSON.stringify(json, null, 2) + "\n");
  index.push({ name, kind, file });
}
await writeFile(path.join(outDir, "index.json"), JSON.stringify(index, null, 2) + "\n");
console.log(`wrote ${index.length} fixtures to ${path.relative(process.cwd(), outDir)}`);
