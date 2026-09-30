// Golden corpora for the Markdown port (src/md/**, src/diff3.rs): runs icloud-md
// 0.6.2's own (unmodified) code and node-diff3 over the real fixtures, a
// hand-picked edge-case list, and seeded fuzz inputs, and writes what they
// return to tests/golden/*.json.gz. The Rust tests (tests/md_*.rs,
// tests/diff3_*.rs) assert byte-for-byte equality with these.
//
//   ICLOUD_MD=../../../coddingtonbear/icloud-md \
//     $ICLOUD_MD/node_modules/.bin/tsx tests/golden/gen.mts
//
// Deterministic: re-running reproduces the files byte for byte.

import { readFile, writeFile } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");
const icloudMd = path.resolve(process.env.ICLOUD_MD ?? path.join(repoRoot, "../../../coddingtonbear/icloud-md"));
const mod = async (rel: string): Promise<any> => import(pathToFileURL(path.join(icloudMd, rel)).href);

const { renderNoteMarkdown, spellingCandidates } = await mod("src/notes/renderNoteMarkdown.ts");
const { parseNoteMarkdown, countQuoteMarkers } = await mod("src/notes/parseNoteMarkdown.ts");
const { formatsRoundTripEqual, normalizeSpans, trimTrailingWhitespace } = await mod("src/notes/noteFormat.ts");
const { renderMarkdownTable, parseMarkdownTable, findMarkdownTableBlocks } = await mod("src/notes/markdownTable.ts");
const { splitFrontmatter } = await mod("src/notes/frontmatter.ts");
const noteId = await mod("src/notes/noteIdFrontmatter.ts");
const titles = await mod("src/notes/titleFilename.ts");
const names = await mod("src/notes/filename.ts");
const titleParagraph = await mod("src/notes/noteTitleParagraph.ts");
const { mergeNoteVersions, hasConflictMarkers } = await mod("src/notes/mergeConflict.ts");
const diff3 = await mod("node_modules/node-diff3/dist/diff3.mjs");

// --- deterministic randomness ----------------------------------------------

let seed = 0x5eed_c0de;
function rand(): number {
  // mulberry32
  seed = (seed + 0x6d2b79f5) | 0;
  let t = seed;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
}
const int = (n: number): number => Math.floor(rand() * n);
const pick = <T>(xs: readonly T[]): T => xs[int(xs.length)]!;
const chance = (p: number): boolean => rand() < p;

const PLAIN = { bold: false, italic: false, strikethrough: false, underline: false, link: "" };

// Inline text: characters and tokens chosen to hit escaping rules.
const ATOMS = [
  ..."abcxyzABWw019 .,;:!?-+=*_`#>|[]()<>&\\~^@/\"'{}$%",
  "\t", "  ", "é", "日本", "😀", "\u00a0", "\ufffc", "\u2028", "Ω",
  "https://example.com/a_b", "https://x.org/p?q=1&r=2", "http://a.b", "www.example.com", "Www.VJW.digital.go.jp",
  "flow.ts", "me@example.com", "[[Note]]", "[[a|b]]", "![[img.png]]", "[!NOTE]", "[^1]", "^[x]", "#tag", "##", "==hi==",
  "===", "---", "***", "1.", "2)", "- ", "* ", "+ ", "> ", "[ ]", "[x]", "&amp;", "&#x20;", "&", "<u>", "</u>", "<div>",
  "**", "__", "~~", "```", "~~~", "\\*", "a_b_c", "snake_case", "(", ")", "[a](b)", "[a]: http://x", "<http://x>",
];

function randomText(maxAtoms: number): string {
  let s = "";
  const n = int(maxAtoms + 1);
  for (let i = 0; i < n; i += 1) {
    s += pick(ATOMS);
  }
  if (chance(0.1)) s = " " + s;
  if (chance(0.1)) s += pick([" ", "\t", "  "]);
  return s;
}

/** Splits `text` (at code-point boundaries) into styled spans. */
function randomSpans(text: string) {
  const points = [...text];
  const spans: any[] = [];
  let i = 0;
  while (i < points.length) {
    const take = 1 + int(Math.min(points.length - i, 6));
    const piece = points.slice(i, i + take).join("");
    i += take;
    const style = { ...PLAIN };
    if (chance(0.35)) {
      style.bold = chance(0.5);
      style.italic = chance(0.5);
      style.strikethrough = chance(0.25);
      style.underline = chance(0.2);
      if (chance(0.2)) style.link = chance(0.3) ? piece : pick(["https://example.com/", "http://x.y/a(b)", "mailto:" + piece, "https://a.b/c d", "x"]);
    }
    const last = spans[spans.length - 1];
    if (last && chance(0.3)) {
      last.length += piece.length;
    } else {
      spans.push({ ...style, length: piece.length });
    }
  }
  return spans;
}

const KINDS = ["title", "heading", "subheading", "body", "body", "body", "monospaced", "bulletList", "dashList", "numberedList", "todoList"];

function randomParagraph(): any {
  const kind = pick(KINDS);
  const text = chance(0.12) ? "" : randomText(kind === "monospaced" ? 5 : 7);
  const list = kind === "bulletList" || kind === "dashList" || kind === "numberedList" || kind === "todoList";
  return {
    kind,
    indent: list ? pick([0, 0, 0, 1, 1, 2, 3]) : chance(0.05) ? 1 : 0,
    blockQuoteLevel: chance(0.8) ? 0 : pick([1, 1, 2, 3]),
    ...(kind === "todoList" ? { done: chance(0.5) } : {}),
    startNumber: chance(0.8) ? 0 : pick([1, 2, 5, 10, 99]),
    text,
    spans: chance(0.5) ? (text.length > 0 ? [{ ...PLAIN, length: text.length }] : []) : randomSpans(text),
    start: 0,
  };
}

function withStarts(paragraphs: any[]): any[] {
  let offset = 0;
  for (const p of paragraphs) {
    p.start = offset;
    offset += p.text.length + 1;
  }
  return paragraphs;
}

function randomNote(): any[] {
  const n = 1 + int(7);
  const out: any[] = [];
  for (let i = 0; i < n; i += 1) {
    // Runs of the same kind make lists, fences and quotes interesting.
    if (i > 0 && chance(0.35)) {
      const prev = out[i - 1];
      const p = randomParagraph();
      p.kind = prev.kind;
      if (p.kind === "todoList") p.done = chance(0.5);
      else delete p.done;
      p.blockQuoteLevel = prev.blockQuoteLevel;
      out.push(p);
    } else {
      out.push(randomParagraph());
    }
  }
  return withStarts(out);
}

function parseResult(markdown: string): any {
  const r = parseNoteMarkdown(markdown);
  return r.status === "ok" ? { ok: { paragraphs: r.paragraphs, text: r.text } } : { unsupported: r.reason };
}

// --- render ----------------------------------------------------------------

const fixtureIndex = JSON.parse(await readFile(path.join(repoRoot, "tests/fixtures/real/index.json"), "utf8"));
const realFormats: any[][] = [];
for (const entry of fixtureIndex) {
  if (entry.kind !== "note") continue;
  const fixture = JSON.parse(await readFile(path.join(repoRoot, "tests/fixtures/real", entry.file), "utf8"));
  if (Array.isArray(fixture.golden?.format)) realFormats.push(fixture.golden.format);
}

const renderCases: any[] = [];
const parseCases: any[] = [];
const projectionCases: any[] = [];
const addRender = (paragraphs: any[], source: string) => {
  // A lone surrogate (an attention run next to an astral character) is
  // written to disk as U+FFFD; Rust strings hold only the latter.
  const markdown = renderNoteMarkdown(paragraphs).toWellFormed();
  renderCases.push({ source, paragraphs, markdown });
  parseCases.push({ source: "render", markdown, result: parseResult(markdown) });
};
realFormats.forEach((format, i) => addRender(format, `real-${i}`));
// Each real note's paragraphs one at a time and in sliding windows.
for (const format of realFormats) {
  for (let i = 0; i < format.length; i += 1) {
    addRender(withStarts(format.slice(i, i + 3).map((p: any) => ({ ...p }))), "real-window");
  }
}
for (let i = 0; i < 4000; i += 1) {
  addRender(randomNote(), "fuzz");
}

// Projection helpers (noteFormat.ts) over the same inputs.
for (let i = 0; i < 600; i += 1) {
  const a = randomNote();
  const b = chance(0.5) ? a.map((p: any) => ({ ...p, spans: randomSpans(p.text) })) : randomNote();
  projectionCases.push({
    a,
    b,
    equal: formatsRoundTripEqual(a, b),
    normalized: a.map((p: any) => normalizeSpans(p)),
    trimmed: a.map((p: any) => trimTrailingWhitespace(p)),
  });
}

// --- parse -------------------------------------------------------------------

const HANDWRITTEN = [
  "", "\n", "\n\n", "a", "a\n", "# T", "#### too deep", "a\n\n---\n\nb", "<div>\nblock\n</div>", "some `inline code` here",
  "> quoted\nlazy line", "* item", "Title\n=====", "Sub\n---", "# **Outfits**&#x20;\nplain trailing \nFried Egg&#x20;",
  "- [ ]", "- [x]", "- [X]", "* [ ]", "> - [ ]", "- [ ] ", "- \\[ ]", "1. a\n2. b", "5. five\n6. six", "3) x", "- a\n\n- b",
  "- a\n  - b\n    - c", "- a\n  1. b\n- c", "> a\n>\n> b", ">> deep", "> > spaced", "   > three", "    code", "```\ncode\n```",
  "```js\nx\n```", "~~~\nx\n~~~", "```\nunclosed", "````\n```\n````", "| a | b |\n| - | - |\n| c | d |", "[a]: http://x", "[^1]: note",
  "text[^1]\n\n[^1]: n", "![img](x.png)", "[link](http://a \"title\")", "<http://a.b>", "www.example.com", "a@b.co", "**b** _i_ ~~s~~ ~t~",
  "<u>under</u>", "<U>x</U>", "<u>open", "a</u>", "<b>bold</b>", "hard  \nbreak", "back\\\nslash", "&amp; &copy; &#65;", "\\*not\\*",
  "- a\nlazy", "1. a\nlazy", "- a\n\n  para", "- ```\n  code\n  ```", "- # h", "- > q", "-\n  x", "- ", "-", "1.", "* * *",
  "a\n===", "a\n---", "  indented para", "\ta", "a\r\nb", "a\rb", "\ufffc", "x\u2028y", "# ", "#", "##", "# a #", "*a*b*", "__a__",
  "[[Note]]", "[[a|b]]", "[!NOTE]", "> [!TIP]- x", "#tag", "==h==", "- [ ] [[x]]", "https://a.b/c_d", "[a]\n\n[a]: /u", "[a][]",
  "| a |\n| - |", "a | b\n- | -", "  - a", "10. ten", "0. zero", "1234567890. big", "- a\n- b\n\n\n- c", "> - a\n> - b", "> 1. x",
  "- a\n    - b", "- a\n      - b", "***bold italic***", "*a **b** c*", "**a *b* c**", "*a*\n*b*", "`a\nb`", "<!-- c -->", "<br>",
  "😀_foo_", "_foo_😀", "😀*(x)*", "*(x)*😀", "😀 *x*", "**😀**", "_😀_", "a😀_b_", "~~😀~~", "😀~x~", "# #tag", "## #a #b", "# # a",
  "- x\n\na\n-", ">\na\n-", "1.\na\n-", "    code\n-", "    code\n2. x", "- [x]\né", "> - [ ]\n>\n- )", "[http://x\\`](y)",
  "- a\n\tb", "- [ ] x\n\ty", "text\n\tmore", "- a\n  - b\n\n  c", "a\n\n\n\nb", "> a\n>\n>\n> b",
];
for (const markdown of HANDWRITTEN) {
  parseCases.push({ source: "handwritten", markdown, result: parseResult(markdown) });
}
const LINE_STARTS = ["", "", "", "# ", "## ", "### ", "#### ", "- ", "* ", "+ ", "1. ", "3) ", "> ", ">> ", "> > ", "- [ ] ", "- [x] ", "  ", "    ", "\t", "  - ", "    - ", "1. ", "   1. "];
const LINE_WHOLE = ["```", "~~~", "---", "***", "___", "===", "<div>", "</div>", "| a | b |", "| - | - |", "[a]: http://x", "[^1]: note", "", "", "- [ ]", ">", "-", "  "];
const INLINE = ["`code`", "![i](x)", "[l](http://a)", "<http://a>", "**b**", "_i_", "~~s~~", "~s~", "<u>u</u>", "&amp;", "&#x20;", "\\*", "[^1]", "www.a.com", "a@b.co", "<b>", "  ", "\\"];
for (let i = 0; i < 2500; i += 1) {
  const n = 1 + int(8);
  const lines: string[] = [];
  for (let j = 0; j < n; j += 1) {
    if (chance(0.25)) {
      lines.push(pick(LINE_WHOLE));
    } else {
      let line = pick(LINE_STARTS);
      const k = int(4);
      for (let m = 0; m < k; m += 1) line += chance(0.5) ? pick(INLINE) : randomText(2);
      lines.push(line);
    }
  }
  const markdown = lines.join("\n");
  parseCases.push({ source: "soup", markdown, result: parseResult(markdown) });
}

const miscCases = {
  countQuoteMarkers: ["", ">", "> a", ">>", "> > a", "   > a", "    > a", " >  > a", ">>>x", "a > b"].map((line) => ({
    line,
    count: countQuoteMarkers(line),
  })),
  spellingCandidates: Array.from({ length: 300 }, () => {
    const lines = Array.from({ length: 1 + int(3) }, () => randomText(5));
    return { lines, candidates: spellingCandidates(lines) };
  }),
};

// --- tables --------------------------------------------------------------------

const CELL_ATOMS = [...ATOMS, "\n", "\r\n", "|", "[[a|b]]", "a|b", "<br>", "   ", "`x|y`"];
const tableRender: any[] = [];
const tableParse: any[] = [];
const tableFind: any[] = [];
const tryCall = (f: () => any) => {
  try {
    return { ok: f() };
  } catch (e: any) {
    return { error: e.message };
  }
};
for (let i = 0; i < 1500; i += 1) {
  const rows = int(4) + (chance(0.97) ? 1 : 0);
  const cols = 1 + int(4);
  const grid: string[][] = [];
  for (let r = 0; r < rows; r += 1) {
    const row: string[] = [];
    for (let c = 0; c < cols; c += 1) {
      let cell = "";
      const k = int(4);
      for (let m = 0; m < k; m += 1) cell += pick(CELL_ATOMS);
      row.push(cell);
    }
    grid.push(row);
  }
  const rendered = tryCall(() => renderMarkdownTable(grid));
  tableRender.push({ grid, rendered });
  if (rendered.ok !== undefined) {
    tableParse.push({ markdown: rendered.ok, result: tryCall(() => parseMarkdownTable(rendered.ok)) });
  }
}
const TABLE_TEXTS = [
  "| a | b |\n| - | - |\n| c | d |", "| a |\n| - |", "a | b\n- | -", "| a | b |\n| - | - |\n| c |", "| a | b |\n| - | - |\ntrailing",
  "\n| a |\n| - |\n\n", "| a |\n| - |\n\n| b |\n| - |", "| a |\n|---|\n| x |", "|a|\n|-|\n|b|", "| a | b |\n| :- | -: |\n| c | d |",
  "| `a|b` |\n| - |", "| a \\| b |\n| - |", "| a<br>b |\n| - |", "| a<br/>b |\n| - |", "| a<BR >b |\n| - |", "| **b** |\n| - |",
  "| https://a.b/c_d |\n| - |", "| www.a.com |\n| - |", "| [x](y) |\n| - |", "| &amp; |\n| - |", "", "| a |", "| - |",
  "> | a |\n> | - |", "- | a |\n  | - |", "text\n| a |\n| - |\nmore", "| a | b |\n| - | - |\n| c | d | e |",
];
for (const text of TABLE_TEXTS) {
  tableParse.push({ markdown: text, result: tryCall(() => parseMarkdownTable(text)) });
  tableFind.push({ text, blocks: tryCall(() => findMarkdownTableBlocks(text)) });
}
for (let i = 0; i < 600; i += 1) {
  const parts: string[] = [];
  const n = 1 + int(5);
  for (let j = 0; j < n; j += 1) {
    if (chance(0.5)) {
      const g = tableRender[int(tableRender.length)]!;
      if (g.rendered.ok !== undefined) parts.push(g.rendered.ok);
    } else {
      parts.push(pick([...LINE_WHOLE, randomText(4), "", "| x |", "| x | y |", "|---|", "a | b"]));
    }
  }
  const text = parts.join("\n");
  tableFind.push({ text, blocks: tryCall(() => findMarkdownTableBlocks(text)) });
}

// --- frontmatter -----------------------------------------------------------------

const ID = "03667D1D-EEE8-4E98-82FB-8C5CD02FD9D1";
const IDS = [ID, ID.toLowerCase(), "not-an-id", "03667D1D-EEE8-4E98-82FB-8C5CD02FD9D", "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1x"];
const TITLES = [
  "A title", "", " ", "123", "1.5", "true", "null", "~", "yes", "A: title", "# hash", "a # b", "it's", "say \"hi\"", "both ' and \"",
  "- dash", "? q", "[x]", "{y}", "*star", "&amp", "!bang", "|pipe", ">gt", "%pct", "@at", "`tick", "trailing ", " leading", "tab\there",
  "back\\slash", "é日本😀", "\u0007bell", "\u007fdel", "x\u2028y", "---doc", "...dots", "a".repeat(85), `${"word ".repeat(20)}end`,
  `'${"q".repeat(90)}'`, `${"long\"quoted ".repeat(9)}`, "0x1F", "0o17", "1e3", ".inf", "a: b: c", "x:", "http://a.b/c",
];
const YAML_BODIES = [
  "", "title: Foo", `apple-note-id: ${ID}`, `apple-note-id: "${ID}"`, `apple-note-id: '${ID.toLowerCase()}'`, `apple-note-id: 12`,
  `apple-note-id:\n  - x`, "tags: [a, b]", "tags:\n- a\n- b", "tags:\n  - a\n  - b", "aliases: []", "a: {}", "nested:\n    deep: 1\n    other: {a: 1, b: 2}",
  "a: 007\nb: 1.50\nc: 1.\nd: -0\ne: +5\nf: 12345678901234567890\ng: 0.1\nh: 1.0e3\ni: True\nj: ~\nk: NULL\nl: 1e21\nm: 0x1F\nn: .NaN\no: -.inf",
  "a: 1\n\n\nb: 2", "\n\na: 1", "a: 1\n\n", "a: 1\nb:\n\n  - x\n\n  - y\n", "k : v", "a: 'it''s'\nb: \"x\\ty\"\nc: \"\\u00e9\"",
  "a: b: c", "a: 1\na: 2", "# comment\ntitle: x", "title: x # trailing", "x: |\n  line1\n  line2", "x: >\n  folded\n  text", "foo", "- a\n- b",
  "a: &anchor 1\nb: *anchor", "a: !tag x", "long: " + "aaaa ".repeat(20), "q: 'quoted " + "word ".repeat(20) + "'", "e: \"\"", "e: ''",
  "date: 2024-01-01", "created: 2024-01-01T10:00:00", "cssclass: wide", "tags: [a, \"b c\", 'd']", "t: [a, [b]]", "m: {a: 1, b: [x]}",
  "a:\n  b:\n    c: 1", "a:\n\n  b: 1", "a:\n  - x\n\nb: 1", `apple-note-title: Old\napple-note-id: ${ID}`, `apple-note-title: "Old"`,
  "x: |\n  a\n  b\ny: 1", "x: |-\n  a", "x: |+\n  a\n\n", "x: >\n  a\n  b\n\n  c", "x: >-\n  " + "folded words ".repeat(12),
  "x: |\n  " + "literal words ".repeat(12), "x: |\n   indented\n  less", "x: >\n  a\n    more", "x: |\n\n  after blank", "x: |2\n  a",
  "t: [a,\n  b]", "t: [unclosed\nk: v", `notes: |\n  apple-note-id: ${ID}`, `apple-note-id: >-\n  ${ID}`, `aliases:\n  - apple-note-id: ${ID}`,
  "# c\ntitle: x", "# c\n\ntitle: x", "a: 1 # c", "a: |\n  x\n\nb: 1", `apple-note-title: |\n  Old\napple-note-id: ${ID}`,
  `apple-note-title: >\n  Old\n  title`, "just a bare scalar", ": : :", "a:\n  - x\n  -\n  - [a, b]", "m:\n  k: [x,\n    y]",
  `t: "a long double quoted value that goes on\n  and continues\n\n  after a blank"`, "t: 'single\n  continued'", `t: "esc\\\n  aped"`,
  `apple-note-title: "A title far too long to be a file name, A title far too long to be a\n  file name, "`,
  "key: value   ", "a:    spaced", "url: http://x.y/z", "colon: a:b", "t: [ a , b ]", "t: [a, b,]", "empty:\nnext: 1", "x: -1\ny: - 1",
];
function yamlGen(): string {
  const n = 1 + int(4);
  const out: string[] = [];
  for (let i = 0; i < n; i += 1) {
    const key = pick(["title", "tags", "aliases", "date", "k" + i, "apple-note-id", "apple-note-title", "cssclass"]);
    if (out.some((l) => l.startsWith(key + ":"))) continue;
    const shape = int(6);
    const scalar = () => pick(["x", "Foo Bar", "'q'", '"d"', "12", "1.50", "true", "~", "", ID, "a-b", "[]", "{}", "é"]);
    if (chance(0.15)) out.push("");
    if (shape === 0) out.push(`${key}:`);
    else if (shape === 1) out.push(`${key}: [${Array.from({ length: int(4) }, scalar).filter((s) => s !== "").join(", ")}]`);
    else if (shape === 2) {
      out.push(`${key}:`);
      const indent = pick(["", "  ", "    "]);
      for (let j = 0; j < 1 + int(3); j += 1) out.push(`${indent}- ${scalar() || "x"}`);
    } else out.push(`${key}: ${scalar()}`);
  }
  return out.join("\n");
}
const fmInputs: string[] = [];
for (const body of YAML_BODIES) {
  fmInputs.push(`---\n${body}\n---\n\n`);
  fmInputs.push(`---\n${body}\n---\n`);
}
fmInputs.push("", "   ", "\n", "---\n---\n", "---\n\n---\n\n\n", "---\nno closing", "--- \nx: 1\n---\n", "notfm");
for (let i = 0; i < 400; i += 1) fmInputs.push(`---\n${yamlGen()}\n---\n${pick(["", "\n", "\n\n"])}`);
const frontmatterCases = fmInputs.map((fm) => {
  const id = pick(IDS);
  const title = pick(TITLES);
  const composeTitle = chance(0.5) ? title : undefined;
  return {
    frontmatter: fm,
    id,
    title,
    readNoteId: noteId.readNoteId(fm) ?? null,
    readNoteTitle: noteId.readNoteTitle(fm) ?? null,
    setNoteId: noteId.setNoteId(fm, id),
    setNoteIdValid: noteId.setNoteId(fm, ID.toLowerCase()),
    clearNoteId: noteId.clearNoteId(fm),
    setNoteTitle: noteId.setNoteTitle(fm, title),
    clearNoteTitle: noteId.clearNoteTitle(fm),
    composeTitle,
    composeNoteFile: noteId.composeNoteFile(fm, "body\n", ID, composeTitle),
  };
});
const SPLIT_TEXTS = [
  "", "---", "---\n", "---\n---", "---\n---\n", "---\na: 1\n---", "---\na: 1\n---\n", "---\na: 1\n---\n\n\n# T", "---\na: 1\n---\n# T",
  "---\n\n---\nbody", "---\n---\nbody", "--- \na\n---\n", "x\n---\na\n---", "---\na\n", "---\na: 1\n---\n\n", "---\na: 1\n---\n\n\n",
  "---\r\na\r\n---\r\n", "---\na: 1\n---\nbody\n---\nmore",
];
const splitCases = SPLIT_TEXTS.flatMap((text) => [false, true].map((filenameAsTitle) => ({ text, filenameAsTitle, split: splitFrontmatter(text, { filenameAsTitle }) })));

// --- names -------------------------------------------------------------------------

const NAME_ATOMS = [..."abcXYZ019 ./\\:*?\"<>|#^[]-_'", "\t", "\n", "  ", "é", "日", "😀", "⁄", "꞉", "？", "❘", "＃", "［", "］", "⧵", "∗", "”", "‹", "›", "＾", "\u2060", "\u00a0", "\ufeff", "\u0085", "CON", "nul", "COM1", "LPT9", "Untitled"];
const randomName = (max: number) => Array.from({ length: int(max + 1) }, () => pick(NAME_ATOMS)).join("");
const nameCases: any[] = [];
for (let i = 0; i < 1500; i += 1) {
  const title = chance(0.05) ? "x".repeat(55 + int(10)) : randomName(10);
  const fileVariants = [
    names.noteFileNameFor(title, "filename"),
    names.noteFileNameFor(title, "filename").replace(/\.md$/, ` ${pick(["2", "1", "02", "10", "x", ""])}.md`),
    names.noteFileNameFor(randomName(4), "filename"),
    "Untitled.md",
    "Untitled 3.md",
  ];
  const fileName = pick(fileVariants);
  nameCases.push({
    title,
    fileName,
    noteFileName: names.noteFileName(title),
    inBody: names.noteFileNameFor(title, "in-body"),
    filename: names.noteFileNameFor(title, "filename"),
    needingInBody: names.titleNeedingFrontmatter(title, "in-body") ?? null,
    needingFilename: names.titleNeedingFrontmatter(title, "filename") ?? null,
    carries: names.fileNameCarriesTitle(fileName, title),
    encoded: titles.encodeTitleStem(title),
    decoded: titles.decodeTitleStem(title),
    problem: titles.representabilityProblem(title) ?? null,
    carried: titles.carriedTitleSpelling(title),
    fromFile: titleParagraph.titleFromNoteFileName(`Notes/${fileName}`),
  });
}
const uniqueCases: any[] = [];
for (let i = 0; i < 300; i += 1) {
  const base = pick(["New Note.md", "a.md", "x", ".md", "a.b.md", "Foo 2.md", "日本.md"]);
  const used = new Set<string>();
  const stem = base.replace(/\.md$/, "");
  const k = int(5);
  for (let j = 0; j < k; j += 1) used.add(pick([base, `${stem} 2.md`, `${stem} 3.md`, `${stem} 2`, `${stem} 4.md`, "other.md"]));
  uniqueCases.push({ fileName: base, used: [...used].sort(), result: names.uniqueFileName(base, used) });
}

// --- diff3 ---------------------------------------------------------------------------

const LINE_POOL = ["a", "b", "c", "d", "e", "", "# T", "x", "y", "constructor", "__proto__", "toString"];
const randomLines = (n: number) => Array.from({ length: n }, () => pick(LINE_POOL));
function mutate(lines: string[]): string[] {
  const out = [...lines];
  const edits = int(4);
  for (let i = 0; i < edits; i += 1) {
    const at = int(out.length + 1);
    const op = int(3);
    if (op === 0) out.splice(at, 0, ...randomLines(1 + int(2)));
    else if (op === 1 && out.length > 0) out.splice(Math.min(at, out.length - 1), 1 + int(2));
    else if (out.length > 0) out[Math.min(at, out.length - 1)] = pick(LINE_POOL);
  }
  return out;
}
const diff3Cases: any[] = [];
for (let i = 0; i < 3000; i += 1) {
  const base = randomLines(int(9));
  const local = chance(0.2) ? [...base] : mutate(base);
  const remote = chance(0.2) ? [...base] : chance(0.1) ? [...local] : mutate(base);
  const b = base.join("\n");
  const l = local.join("\n");
  const r = remote.join("\n");
  diff3Cases.push({
    base: b,
    local: l,
    remote: r,
    lines: { base, local, remote },
    merged: mergeNoteVersions(b, l, r),
    plain: diff3.mergeDiff3(local, base, remote, { excludeFalseConflicts: false }),
    comm: diff3.diffComm(local, remote),
    indices: diff3.diffIndices(base, local).map((h: any) => ({ buffer1: h.buffer1, buffer2: h.buffer2 })),
  });
}
const markerCases = [
  "", "<<<<<<<", "<<<<<<< local", "<<<<<<<x", "|||||||", "||||||| base", "=======", "======= x", ">>>>>>>", ">>>>>>> remote",
  "a\n=======\nb", "a\r=======", "<<<<<<<<", " <<<<<<<", "x\u2028=======", "=======\r\n",
].map((text) => ({ text, has: hasConflictMarkers(text) }));

// --- write ---------------------------------------------------------------------------

// Gzipped (zlib's header carries no timestamp, so the bytes are stable).
const write = async (name: string, data: unknown) =>
  writeFile(
    path.join(here, `${name}.gz`),
    gzipSync(JSON.stringify(data, (_key, value) => (typeof value === "string" ? value.toWellFormed() : value)) + "\n", { level: 9 }),
  );
await write("md_render.json", renderCases);
await write("md_parse.json", parseCases);
await write("md_projection.json", projectionCases);
await write("md_misc.json", miscCases);
await write("md_table.json", { render: tableRender, parse: tableParse, find: tableFind });
await write("md_frontmatter.json", { split: splitCases, ops: frontmatterCases });
await write("md_names.json", { names: nameCases, unique: uniqueCases });
await write("diff3.json", { merges: diff3Cases, markers: markerCases });
console.log(
  `render ${renderCases.length}, parse ${parseCases.length}, projection ${projectionCases.length}, table ${tableRender.length}/${tableParse.length}/${tableFind.length}, frontmatter ${splitCases.length}/${frontmatterCases.length}, names ${nameCases.length}/${uniqueCases.length}, diff3 ${diff3Cases.length}/${markerCases.length}`,
);
