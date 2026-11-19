import { execFile, spawn } from "node:child_process";
import { accessSync, constants, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

// Everything here talks to the local `zone` program. Arguments always go to the program as an
// array, never through a shell, so nothing a person types is ever read as a command. This file
// does not import Raycast, so the tests can run it against a throwaway library; zone.ts wires it
// to the extension's preferences.

export type Kind = "dictation" | "note" | "meeting" | "clip";
export const KINDS: Kind[] = ["dictation", "note", "meeting", "clip"];

export interface Segment {
  speaker?: string;
  start?: number;
  end?: number;
  text?: string;
}

export interface ZoneItem {
  id: string;
  kind: Kind;
  source: string | null;
  created_at: number;
  updated_at: number;
  created_local: string;
  text: string;
  title: string | null;
  duration_s: number | null;
  words: number;
  engine: string | null;
  status: string | null;
  extra: { segments?: Segment[] } & Record<string, unknown>;
  processed_at: number | null;
  carrier: string | null;
  device_id: string | null;
  received_at: number | null;
  snippet?: string;
}

export interface ItemPage {
  total: number;
  items: ZoneItem[];
  next_cursor?: string | null;
}

export interface Clip {
  id: string;
  source: string | null;
  created_at: number;
  created_local: string;
  received_at: number;
  age_s: number;
  carrier: string | null;
  device_id: string | null;
  length: number;
  bytes: number;
  lines: number;
  first_line: string;
  text: string;
}

export interface StatusPhone {
  name: string;
  app: string | null;
  endpoint_id: string;
  paired: string | null;
  last_sync: string | null;
  last_sync_age_s: number | null;
  items_received: number;
  last_path: string | null;
}

export interface SyncMoment {
  time: string;
  age_s: number;
}

export interface ZoneStatus {
  now: { local: string; date: string; weekday: string; timezone: string; utc_offset: string; rfc3339: string };
  computer: { name: string; os: string; zone_version: string };
  daemon_running: boolean;
  daemon?: {
    running: boolean;
    pid?: number | null;
    version?: string | null;
    web_page?: string | null;
    relay_connected?: boolean | null;
  } | null;
  phones: StatusPhone[];
  last_sync: { link: SyncMoment | null; flash: SyncMoment | null; backup_folder: SyncMoment | null };
  backup_folder: string | null;
  items: {
    total: number;
    by_kind: Partial<Record<Kind, number>>;
    dictations_by_source: Record<string, number>;
    recordings_behind_notes?: number;
    unprocessed: number;
    oldest: string | null;
    newest: string | null;
  };
  data_directory: string;
}

export const INSTALL_URL = "https://github.com/zaydiscold/zone-mcp#install";
export const APP_STORE_URL = "https://apps.apple.com/app/id6830955448";
export const INSTALL_COMMAND = "curl -fsSL https://raw.githubusercontent.com/zaydiscold/zone-mcp/main/install.sh | sh";

/** zone could not be found where the preference or the usual places say. */
export class ZoneNotFoundError extends Error {
  readonly configuredPath?: string;
  constructor(configuredPath?: string) {
    super(configuredPath ? `zone is not at ${configuredPath}` : "zone is not installed");
    this.name = "ZoneNotFoundError";
    this.configuredPath = configuredPath;
  }
}

/** zone ran and said no. `message` is the first line it printed on standard error. */
export class ZoneCommandError extends Error {
  readonly exitCode: number | null;
  constructor(message: string, exitCode: number | null) {
    super(message);
    this.name = "ZoneCommandError";
    this.exitCode = exitCode;
  }
}

export function isZoneNotFound(error: unknown): error is ZoneNotFoundError {
  return error instanceof ZoneNotFoundError;
}

export function isNotRunning(error: unknown): boolean {
  return error instanceof ZoneCommandError && /not running/i.test(error.message);
}

export interface ZoneSettings {
  /** The zone Path preference: empty to look in the usual places. */
  zonePath?: string;
  /** The Library Folder preference: zone's ZONE_HOME, empty for the usual one. */
  libraryFolder?: string;
}

let readSettings: () => ZoneSettings = () => ({});

/** Where settings come from: the extension's preferences, or a test's. */
export function setSettingsSource(source: () => ZoneSettings) {
  readSettings = source;
}

function expandHome(path: string): string {
  return path === "~" || path.startsWith("~/") ? join(homedir(), path.slice(1)) : path;
}

function isExecutable(path: string): boolean {
  try {
    accessSync(path, constants.X_OK);
    return statSync(path).isFile();
  } catch {
    return false;
  }
}

export function candidatePaths(): string[] {
  return [
    join(homedir(), ".local/bin/zone"),
    "/usr/local/bin/zone",
    "/opt/homebrew/bin/zone",
    "/Applications/Zone.app/Contents/Helpers/zone",
    join(homedir(), "Applications/Zone.app/Contents/Helpers/zone"),
  ];
}

/** The zone program to run: the preference when set, else the first of the usual places. */
export function findZone(): string {
  const configured = readSettings().zonePath?.trim();
  if (configured) {
    const path = expandHome(configured);
    if (isExecutable(path)) return path;
    throw new ZoneNotFoundError(path);
  }
  const found = candidatePaths().find(isExecutable);
  if (!found) throw new ZoneNotFoundError();
  return found;
}

function zoneEnv(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ZONE_NO_UPDATE_CHECK: "1" };
  const folder = readSettings().libraryFolder?.trim();
  if (folder) env.ZONE_HOME = expandHome(folder);
  return env;
}

export interface RunOptions {
  signal?: AbortSignal;
  /** Exit codes that are answers rather than failures. */
  okCodes?: number[];
}

/** Runs zone with these arguments and returns what it printed. */
export function runZone(args: string[], options: RunOptions = {}): Promise<string> {
  const program = findZone();
  return new Promise((resolve, reject) => {
    execFile(
      program,
      args,
      { env: zoneEnv(), maxBuffer: 256 * 1024 * 1024, timeout: 30_000, signal: options.signal },
      (error, stdout, stderr) => {
        if (!error) return resolve(stdout);
        const code = typeof error.code === "number" ? error.code : null;
        if (code !== null && options.okCodes?.includes(code)) return resolve(stdout);
        if ((error as NodeJS.ErrnoException).code === "ENOENT") return reject(new ZoneNotFoundError(program));
        if (error.name === "AbortError") return reject(error);
        const said = stderr
          .split("\n")
          .map((line) => line.replace(/^zone:\s*/, "").trim())
          .find(Boolean);
        reject(new ZoneCommandError(said ?? error.message, code));
      },
    );
  });
}

async function runJson<T>(args: string[], options?: RunOptions): Promise<T> {
  return JSON.parse(await runZone([...args, "--json"], options)) as T;
}

/** A time zone understands: RFC 3339 in UTC with microseconds. */
export function timeArg(unixSeconds: number): string {
  const whole = Math.floor(unixSeconds);
  let micros = Math.round((unixSeconds - whole) * 1e6);
  let seconds = whole;
  if (micros >= 1e6) {
    seconds += 1;
    micros -= 1e6;
  }
  const iso = new Date(seconds * 1000).toISOString().replace(/\.\d{3}Z$/, "");
  return `${iso}.${String(micros).padStart(6, "0")}Z`;
}

export interface ListFilter {
  kinds?: Kind[];
  sources?: string[];
  unprocessed?: boolean;
  since?: string;
  until?: string;
  order?: "newest" | "oldest";
}

function filterArgs(filter: ListFilter): string[] {
  const args: string[] = [];
  for (const kind of filter.kinds ?? []) args.push("--kind", kind);
  for (const source of filter.sources ?? []) args.push("--source", source);
  if (filter.unprocessed) args.push("--unprocessed");
  if (filter.since) args.push("--since", filter.since);
  if (filter.until) args.push("--until", filter.until);
  return args;
}

/**
 * Where the next page of a list starts. `zone list` has no cursor, so pages are cut by time: the
 * next page asks for items up to (newest first) or from (oldest first) the last item's time, and
 * drops the items at exactly that time that the previous page already showed.
 */
export interface ListCursor {
  at: number;
  seen: string[];
}

export interface ListPage {
  total: number;
  items: ZoneItem[];
  cursor?: ListCursor;
  hasMore: boolean;
}

export async function listItems(
  filter: ListFilter,
  limit: number,
  cursor?: ListCursor,
  signal?: AbortSignal,
): Promise<ListPage> {
  const order = filter.order ?? "newest";
  const bounded: ListFilter = { ...filter };
  if (cursor) {
    // A microsecond past the last time, so items that share it are listed again and skipped here.
    if (order === "newest") bounded.until = timeArg(cursor.at + 1e-6);
    else bounded.since = timeArg(cursor.at);
  }
  const fetchCount = limit + (cursor?.seen.length ?? 0);
  const page = await runJson<ItemPage>(
    ["list", ...filterArgs(bounded), "--order", order, "--limit", String(fetchCount)],
    { signal },
  );
  const seen = new Set(cursor?.seen ?? []);
  const items = page.items.filter((item) => !seen.has(item.id));
  const last = items.at(-1);
  const hasMore = page.items.length >= fetchCount && last !== undefined;
  let next: ListCursor | undefined;
  if (last) {
    const sameTime = page.items.filter((item) => item.created_at === last.created_at).map((item) => item.id);
    next = { at: last.created_at, seen: sameTime };
  }
  return { total: page.total, items, cursor: next, hasMore };
}

export interface SearchPage {
  total: number;
  items: ZoneItem[];
  nextCursor: string | null;
}

export async function searchItems(
  query: string,
  filter: ListFilter,
  limit: number,
  cursor?: string | null,
  signal?: AbortSignal,
): Promise<SearchPage> {
  const args = ["search", ...filterArgs(filter), "--limit", String(limit)];
  if (filter.order) args.push("--order", filter.order);
  if (cursor) args.push("--cursor", cursor);
  // `--` keeps a query that starts with a dash from being read as an option.
  const page = JSON.parse(await runZone([...args, "--json", "--", query], { signal })) as ItemPage;
  return { total: page.total, items: page.items, nextCursor: page.next_cursor ?? null };
}

/** Items in full by id. An id zone does not know is reported in `missing`, not thrown. */
export async function showItems(
  ids: string[],
  signal?: AbortSignal,
): Promise<{ items: ZoneItem[]; missing: string[] }> {
  if (ids.length === 0) return { items: [], missing: [] };
  try {
    return { items: await runJson<ZoneItem[]>(["show", ...ids], { signal }), missing: [] };
  } catch (error) {
    if (!(error instanceof ZoneCommandError) || ids.length === 1) {
      if (error instanceof ZoneCommandError) return { items: [], missing: ids };
      throw error;
    }
  }
  const items: ZoneItem[] = [];
  const missing: string[] = [];
  for (const id of ids) {
    const one = await showItems([id], signal);
    items.push(...one.items);
    missing.push(...one.missing);
  }
  return { items, missing };
}

export async function latestClip(signal?: AbortSignal): Promise<Clip | null> {
  const answer = await runJson<{ clip: Clip | null }>(["clip"], { signal });
  return answer.clip;
}

export function getStatus(signal?: AbortSignal): Promise<ZoneStatus> {
  return runJson<ZoneStatus>(["status"], { signal });
}

/** The page's address with its key, for a view of the page such as `#/notes`. */
export async function pageUrl(route = "", flash = false): Promise<string> {
  const args = ["open", "--print"];
  if (flash) args.push("--flash");
  const url = (await runZone(args)).trim();
  return route ? `${url}#${route}` : url;
}

export function itemRoute(item: Pick<ZoneItem, "id" | "kind">): string {
  return item.kind === "meeting" ? `/meeting/${item.id}` : `/item/${item.id}`;
}

interface McpResult {
  content?: { type: string; text?: string }[];
  isError?: boolean;
}

/**
 * Sets or clears the done flag. zone's command line has no command for it, so this asks zone's
 * own MCP server (`zone mcp`, its `mark_processed` tool), the same write an agent or the page's
 * Done makes. Nothing else in the library is touched.
 */
export async function setDone(ids: string[], done: boolean): Promise<{ updated: number; not_found: string[] }> {
  const program = findZone();
  const result = await new Promise<McpResult>((resolve, reject) => {
    const child = spawn(program, ["mcp"], { env: zoneEnv(), stdio: ["pipe", "pipe", "pipe"] });
    let buffer = "";
    let stderr = "";
    let settled = false;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.stdin.end();
      child.kill();
      fn();
    };
    const timer = setTimeout(
      () => finish(() => reject(new ZoneCommandError("zone did not answer in time", null))),
      15_000,
    );
    child.on("error", (error: NodeJS.ErrnoException) =>
      finish(() => reject(error.code === "ENOENT" ? new ZoneNotFoundError(program) : error)),
    );
    child.stderr.on("data", (chunk: Buffer) => (stderr += chunk.toString()));
    child.on("close", (code) =>
      finish(() => reject(new ZoneCommandError(stderr.trim().split("\n")[0] || `zone mcp stopped (${code})`, code))),
    );
    child.stdout.on("data", (chunk: Buffer) => {
      buffer += chunk.toString();
      let newline: number;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (!line) continue;
        let message: { id?: number; result?: McpResult; error?: { message: string } };
        try {
          message = JSON.parse(line);
        } catch {
          continue;
        }
        if (message.id !== 2) continue;
        if (message.error) finish(() => reject(new ZoneCommandError(message.error!.message, null)));
        else finish(() => resolve(message.result ?? {}));
      }
    });
    const send = (message: object) => child.stdin.write(`${JSON.stringify(message)}\n`);
    send({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "raycast-zone", version: "1.0.0" },
      },
    });
    send({ jsonrpc: "2.0", method: "notifications/initialized" });
    send({
      jsonrpc: "2.0",
      id: 2,
      method: "tools/call",
      params: { name: "mark_processed", arguments: { ids, processed: done } },
    });
  });
  const text = result.content?.find((part) => part.type === "text")?.text ?? "{}";
  if (result.isError) throw new ZoneCommandError(text, null);
  const parsed = JSON.parse(text) as { updated?: number; not_found?: string[] };
  return { updated: parsed.updated ?? 0, not_found: parsed.not_found ?? [] };
}
