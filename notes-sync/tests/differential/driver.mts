// Differential-harness driver: runs the UNMODIFIED icloud-md CLI in-process
// against a cassette of CloudKit answers, recording every request it makes.
// See README.md in this directory for the cassette and request-log formats.
//
//   $ICLOUD_MD/node_modules/.bin/tsx tests/differential/driver.mts \
//     --cassette c.json --requests out/requests.json [--home DIR] [--cwd DIR] \
//     [--now MS] [--deterministic] -- <icloud-md args...>
//
// Exits with icloud-md's own exit code; its stdout/stderr pass through.

import crypto from "node:crypto";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { syncBuiltinESMExports } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");
const icloudMd = path.resolve(process.env.ICLOUD_MD ?? path.join(repoRoot, "../../coddingtonbear/icloud-md"));

// --- arguments -------------------------------------------------------------

interface Options {
  cassette: string;
  requests: string | undefined;
  home: string | undefined;
  cwd: string | undefined;
  now: number | undefined;
  deterministic: boolean;
  args: string[];
}

function parseArgs(argv: string[]): Options {
  const options: Options = {
    cassette: "",
    requests: undefined,
    home: undefined,
    cwd: undefined,
    now: undefined,
    deterministic: false,
    args: [],
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]!;
    const value = (): string => {
      const next = argv[i + 1];
      if (next === undefined) {
        throw new Error(`${arg} needs a value`);
      }
      i += 1;
      return next;
    };
    if (arg === "--") {
      options.args = argv.slice(i + 1);
      break;
    } else if (arg === "--cassette") {
      options.cassette = path.resolve(value());
    } else if (arg === "--requests") {
      options.requests = path.resolve(value());
    } else if (arg === "--home") {
      options.home = path.resolve(value());
    } else if (arg === "--cwd") {
      options.cwd = path.resolve(value());
    } else if (arg === "--now") {
      options.now = Number(value());
    } else if (arg === "--deterministic") {
      options.deterministic = true;
    } else {
      throw new Error(`unknown driver option ${arg} (icloud-md's own arguments go after --)`);
    }
  }
  if (options.cassette === "") {
    throw new Error("--cassette is required");
  }
  return options;
}

const options = parseArgs(process.argv.slice(2));

// --- cassette --------------------------------------------------------------

interface Interaction {
  note?: string;
  request: { method: string; path?: string; url?: string; body?: unknown };
  response: { status?: number; body?: unknown; bodyBase64?: string };
  repeat?: boolean;
}

interface Cassette {
  version: number;
  account: { dsid: string; appleId: string };
  ckdatabasewsUrl?: string;
  validate?: unknown;
  interactions: Interaction[];
}

const cassette = JSON.parse(await readFile(options.cassette, "utf-8")) as Cassette;
if (cassette.version !== 1) {
  throw new Error(`unsupported cassette version ${cassette.version}`);
}
const ckdatabasewsUrl = cassette.ckdatabasewsUrl ?? "https://p00-ckdatabasews.icloud.com:443";
const ckHost = new URL(ckdatabasewsUrl).hostname;
const validateBody = cassette.validate ?? {
  dsInfo: { dsid: cassette.account.dsid, appleId: cassette.account.appleId, fullName: "Differential Harness" },
  webservices: { ckdatabasews: { url: ckdatabasewsUrl, status: "active" } },
};

// --- fake HOME with icloud-md's per-account session files ------------------

const home = options.home ?? (await mkdtemp(path.join(os.tmpdir(), "icloud-md-home-")));
const accountDir = path.join(home, ".config", "icloud-md", "accounts", cassette.account.dsid);
await mkdir(accountDir, { recursive: true, mode: 0o700 });
await writeFile(
  path.join(accountDir, "session.local.json"),
  JSON.stringify(
    {
      cookie: "X-APPLE-WEBAUTH-USER=harness; X-APPLE-WEBAUTH-TOKEN=harness",
      clientId: "00000000-0000-0000-0000-000000000000",
      clientBuildNumber: "2534Project50",
      clientMasteringNumber: "2534B21",
      capturedAt: "2026-01-01T00:00:00.000Z",
    },
    null,
    2,
  ) + "\n",
  { mode: 0o600 },
);
await writeFile(
  path.join(accountDir, "meta.json"),
  JSON.stringify({ appleId: cassette.account.appleId, dsid: cassette.account.dsid }, null, 2) + "\n",
  { mode: 0o600 },
);
process.env.HOME = home;

// --- determinism -----------------------------------------------------------

// One UUID stream shared by `node:crypto`'s randomUUID and Web Crypto's
// (icloud-md uses both): the n-th counted call (1-based) returns
// 00000000-0000-4000-8000-<n as 12 hex digits>. randomBytes(k) on its m-th
// counted call (1-based) returns bytes (m + j) & 0xff for j = 0..k-1. The Rust
// side mirrors both under ICLOUD_NOTES_SYNC_DETERMINISTIC=1.
//
// Only draws made by code the port keeps are counted. A draw whose immediate
// caller is code the port dropped - playwright-core (which draws 9
// randomBytes(16) guids at import time), browser login and the account store
// under src/auth/, session.ts, and setupClient.ts's /validate requestId - gets
// real randomness and leaves both counters alone. `DRIVER_TRACE_DRAWS=1` logs
// every draw with its stack on stderr.
const DROPPED_CALLERS = [
  /\/node_modules\/playwright(-core)?\//,
  /\/src\/auth\//,
  /\/src\/session\.ts:/,
  /\/src\/cloudkit\/setupClient\.ts:/,
];
const traceDraws = process.env.DRIVER_TRACE_DRAWS === "1";

/** The stack below the driver's stub; its first line is the drawing caller. */
function callerStack(): string[] {
  const limit = Error.stackTraceLimit;
  Error.stackTraceLimit = 50;
  const stack = new Error().stack ?? "";
  Error.stackTraceLimit = limit;
  return stack
    .split("\n")
    .slice(1)
    .filter((line) => !line.includes(fileURLToPath(import.meta.url)));
}

/** Whether this draw counts; logs it under DRIVER_TRACE_DRAWS. */
function counted(what: string): boolean {
  const stack = callerStack();
  const caller = stack[0] ?? "";
  const dropped = DROPPED_CALLERS.some((pattern) => pattern.test(caller));
  if (traceDraws) {
    console.error(`[draw] ${what}${dropped ? " (dropped code, not counted)" : ""}\n${stack.join("\n")}`);
  }
  return !dropped;
}

let uuidCounter = 0;
let bytesCounter = 0;
if (options.deterministic) {
  const nodeCrypto = crypto as unknown as Record<string, unknown>;
  const realUuid = crypto.randomUUID.bind(crypto);
  const realBytes = crypto.randomBytes.bind(crypto);
  const nextUuid = (): `${string}-${string}-${string}-${string}-${string}` => {
    if (!counted(`uuid #${uuidCounter + 1}`)) {
      return realUuid();
    }
    uuidCounter += 1;
    return `00000000-0000-4000-8000-${uuidCounter.toString(16).padStart(12, "0")}`;
  };
  nodeCrypto.randomUUID = nextUuid;
  nodeCrypto.randomBytes = (size: number): Buffer => {
    if (!counted(`bytes #${bytesCounter + 1} (${size})`)) {
      return realBytes(size);
    }
    bytesCounter += 1;
    return Buffer.from(Array.from({ length: size }, (_, j) => (bytesCounter + j) & 0xff));
  };
  syncBuiltinESMExports();
  Object.defineProperty(globalThis.crypto, "randomUUID", { value: nextUuid, configurable: true });
}

// A frozen clock: Date.now() and new Date() both answer `--now` (ms epoch).
// Mirrored on the Rust side by ICLOUD_NOTES_SYNC_NOW=<ms>.
if (options.now !== undefined) {
  const RealDate = Date;
  const fixed = options.now;
  class FrozenDate extends RealDate {
    constructor(...args: unknown[]) {
      if (args.length === 0) {
        super(fixed);
      } else {
        super(...(args as [string]));
      }
    }
    static override now(): number {
      return fixed;
    }
  }
  globalThis.Date = FrozenDate as DateConstructor;
}

// --- fetch stub ------------------------------------------------------------

/** Per-session query parameters whose values say nothing about the request. */
const QUERY_NOISE = new Set(["clientId", "clientBuildNumber", "clientMasteringNumber", "dsid", "requestId"]);

interface LoggedRequest {
  method: string;
  service: "setup" | "ckdatabasews" | "other";
  path: string;
  query: Record<string, string>;
  body?: unknown;
  matched: number | null;
}

const log: LoggedRequest[] = [];
const used = new Set<number>();

function parseBody(body: unknown): unknown {
  if (body === undefined || body === null) {
    return undefined;
  }
  const text = typeof body === "string" ? body : Buffer.from(body as ArrayBuffer).toString("utf-8");
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

function findInteraction(method: string, service: LoggedRequest["service"], url: URL, body: unknown): number | undefined {
  return cassette.interactions.findIndex((interaction, index) => {
    if ((used.has(index) && interaction.repeat !== true) || interaction.request.method.toUpperCase() !== method) {
      return false;
    }
    const request = interaction.request;
    if (service === "ckdatabasews") {
      if (request.path !== url.pathname) {
        return false;
      }
    } else if (request.url === undefined) {
      return false;
    } else {
      const wanted = new URL(request.url);
      const sameQuery = wanted.search === "" || wanted.search === url.search;
      if (wanted.origin + wanted.pathname !== url.origin + url.pathname || !sameQuery) {
        return false;
      }
    }
    return request.body === undefined || isDeepStrictEqual(request.body, body);
  });
}

globalThis.fetch = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
  const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
  const method = (init?.method ?? "GET").toUpperCase();
  const body = parseBody(init?.body);
  const service: LoggedRequest["service"] =
    url.hostname === "setup.icloud.com" ? "setup" : url.hostname === ckHost ? "ckdatabasews" : "other";
  const query = Object.fromEntries(
    [...url.searchParams.entries()].filter(([key]) => !QUERY_NOISE.has(key)).sort(([a], [b]) => (a < b ? -1 : 1)),
  );
  const entry: LoggedRequest = {
    method,
    service,
    path: service === "other" ? url.origin + url.pathname : url.pathname,
    query,
    ...(body !== undefined ? { body } : {}),
    matched: null,
  };
  log.push(entry);

  if (service === "setup" && url.pathname === "/setup/ws/1/validate") {
    return new Response(JSON.stringify(validateBody), { status: 200, headers: { "content-type": "application/json" } });
  }

  const index = findInteraction(method, service, url, body);
  if (index === undefined || index < 0) {
    console.error(`[driver] no cassette interaction for ${method} ${entry.path}`);
    return new Response(JSON.stringify({ error: "no cassette interaction matched" }), {
      status: 599,
      headers: { "content-type": "application/json" },
    });
  }
  used.add(index);
  entry.matched = index;
  const response = cassette.interactions[index]!.response;
  if (response.bodyBase64 !== undefined) {
    return new Response(Buffer.from(response.bodyBase64, "base64"), {
      status: response.status ?? 200,
      headers: { "content-type": "application/octet-stream" },
    });
  }
  return new Response(JSON.stringify(response.body ?? {}), {
    status: response.status ?? 200,
    headers: { "content-type": "application/json" },
  });
};

// --- run icloud-md ---------------------------------------------------------

async function writeLog(): Promise<void> {
  if (options.requests !== undefined) {
    await mkdir(path.dirname(options.requests), { recursive: true });
    await writeFile(options.requests, JSON.stringify({ requests: log }, null, 2) + "\n");
  }
}

if (options.cwd !== undefined) {
  process.chdir(options.cwd);
}
process.argv = [process.argv[0]!, "icloud-md", ...options.args];
try {
  // cli.ts parses process.argv and runs the command at module top level,
  // leaving its result in process.exitCode.
  await import(pathToFileURL(path.join(icloudMd, "src", "cli.ts")).href);
} finally {
  await writeLog();
}
process.exit(typeof process.exitCode === "number" ? process.exitCode : Number(process.exitCode ?? 0));
