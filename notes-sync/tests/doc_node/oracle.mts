// icloud-md as an oracle for the `doc` codec tests: reads a JSON array of
// requests on stdin, runs each through icloud-md's own (unmodified) code, and
// writes a JSON array of responses. Bytes travel as base64.
//
//   $ICLOUD_MD/node_modules/.bin/tsx tests/doc_node/oracle.mts < requests.json
//
// Randomness is replaced by queues the request supplies (`uuids` for
// formatReconcile's randomUUID, `randoms` for tableEdit's randomBytes), so a
// run is deterministic and the Rust side can replay the same values.

import { createRequire, syncBuiltinESMExports } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const require = createRequire(import.meta.url);
const crypto = require("node:crypto");
let uuidQueue: string[] = [];
let randomQueue: string[] = [];
crypto.randomUUID = () => {
  const next = uuidQueue.shift();
  if (next === undefined) throw new Error("oracle: randomUUID queue exhausted");
  return next;
};
const realRandomBytes = crypto.randomBytes;
crypto.randomBytes = (size: number) => {
  if (size !== 16) return realRandomBytes(size);
  const next = randomQueue.shift();
  if (next === undefined) throw new Error("oracle: randomBytes queue exhausted");
  return Buffer.from(next, "hex");
};
syncBuiltinESMExports();

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");
const icloudMd = path.resolve(process.env.ICLOUD_MD ?? path.join(repoRoot, "../../coddingtonbear/icloud-md"));
const mod = async (rel: string): Promise<any> => import(pathToFileURL(path.join(icloudMd, "src", rel)).href);

const pb = await import(pathToFileURL(path.join(icloudMd, "node_modules/@bufbuild/protobuf/dist/esm/index.js")).href);
const topotext = await mod("notes/gen/topotext_pb.ts");
const crdt = await mod("notes/gen/crdt_pb.ts");
const versioned = await mod("notes/gen/versioned_document_pb.ts");
const { parseNoteMarkdown } = await mod("notes/parseNoteMarkdown.ts");
const doc = await mod("notes/noteDocument.ts");
const { reconcileNoteFormat } = await mod("notes/formatReconcile.ts");
const { decodeNoteFormat } = await mod("notes/noteFormat.ts");
const { compressNoteDocument, decompressNoteDocument } = await mod("notes/noteText.ts");
const { prepareTableAttachmentUpdate } = await mod("notes/tablePushEdit.ts");
const { classifyNoteRecord } = await mod("notes/decodeNoteRecord.ts");

const SCHEMAS: Record<string, any> = {
  versioned: versioned.DocumentSchema,
  string: topotext.StringSchema,
  crdt: crdt.DocumentSchema,
  attributeRun: topotext.AttributeRunSchema,
};

const b64 = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64");
const unb64 = (s: string) => new Uint8Array(Buffer.from(s, "base64"));
const errorOf = (cause: unknown) => (cause instanceof Error ? cause.message : String(cause));

function handle(request: any): any {
  switch (request.op) {
    case "parseMarkdown":
      return parseNoteMarkdown(request.markdown);
    case "protoRoundTrip": {
      try {
        const message = pb.fromBinary(SCHEMAS[request.schema], unb64(request.b64));
        try {
          return { decoded: true, b64: b64(pb.toBinary(SCHEMAS[request.schema], message)) };
        } catch (cause) {
          return { decoded: true, encodeError: errorOf(cause) };
        }
      } catch (cause) {
        return { decoded: false, error: errorOf(cause) };
      }
    }
    case "noteRoundTrips":
      return doc.noteDocumentRoundTrips(unb64(request.raw));
    case "edit": {
      // A push's edit pipeline on one decompressed note document: per step,
      // applyTextEdit to the step's text, then (when it carries markdown)
      // reconcileNoteFormat to its parsed paragraphs.
      uuidQueue = [...(request.uuids ?? [])];
      const replicaId = new Uint8Array(Buffer.from(request.replicaId, "hex"));
      const results: any[] = [];
      let note: any;
      try {
        note = request.raw !== undefined ? doc.parseNoteDocument(unb64(request.raw)) : undefined;
      } catch (cause) {
        return { parseError: errorOf(cause) };
      }
      for (const step of request.steps) {
        const out: any = {};
        try {
          const parsed = step.markdown !== undefined ? parseNoteMarkdown(step.markdown) : undefined;
          if (parsed !== undefined && parsed.status !== "ok") {
            out.parseRefusal = parsed.reason;
            results.push(out);
            continue;
          }
          const text = parsed !== undefined ? parsed.text : step.text;
          if (note === undefined) {
            note = doc.buildInitialNoteDocument(text, replicaId);
            out.built = true;
          } else {
            out.changed = doc.applyTextEdit(note, text, { replicaId });
          }
          if (parsed !== undefined) {
            out.reconciled = reconcileNoteFormat(note, parsed.paragraphs, replicaId);
          }
          const raw = doc.encodeNoteDocument(note);
          out.raw = b64(raw);
          out.compressed = b64(compressNoteDocument(raw));
        } catch (cause) {
          out.error = errorOf(cause);
        }
        results.push(out);
      }
      return { steps: results, uuidsLeft: uuidQueue.length };
    }
    case "tableEdit": {
      randomQueue = [...(request.randoms ?? [])];
      const record = { recordName: "T", recordType: "Attachment", fields: { MergeableDataEncrypted: { value: request.b64, type: "ENCRYPTED_BYTES" } } };
      const replicaId = new Uint8Array(Buffer.from(request.replicaId, "hex"));
      try {
        return { result: prepareTableAttachmentUpdate(record, request.grid, replicaId), randomsLeft: randomQueue.length };
      } catch (cause) {
        return { thrown: errorOf(cause) };
      }
    }
    case "classify":
      return classifyNoteRecord(request.record, { titleMode: request.titleMode });
    case "decodeFormat": {
      const raw = decompressNoteDocument(Buffer.from(request.compressed, "base64"));
      const note = doc.parseNoteDocument(raw);
      return decodeNoteFormat(note.text, note.attributeRuns);
    }
    default:
      throw new Error(`oracle: unknown op ${request.op}`);
  }
}

const input = await new Promise<string>((resolve) => {
  let data = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => (data += chunk));
  process.stdin.on("end", () => resolve(data));
});
const requests = JSON.parse(input);
process.stdout.write(JSON.stringify(requests.map(handle), (_key, value) => (typeof value === "bigint" ? value.toString() : value)));
