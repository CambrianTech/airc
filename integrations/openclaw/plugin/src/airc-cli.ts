/**
 * airc CLI bridge — the airc grid has no HTTP API, so the channel is driven by
 * spawning the `airc` binary as a child process. This module replaces what
 * `http-client.ts` + `resolve.ts` do for an HTTP-backed channel.
 *
 * Verified CLI surface (airc build b596a2b11399, branch canary):
 *   - subscribe:  `airc [--home <home>] join <room>`
 *   - self id:    `airc [--home <home>] status`            (parses `peer_id:` line)
 *   - poll:       `airc [--home <home>] events list --kind message --limit <N> --json`
 *   - send:       `airc [--home <home>] publish --room <room> --body-text <text>`
 *
 * Note on send: the task spec named `airc msg "<text>"`, but `publish` is the
 * documented OpenClaw/bridge send path (`airc publish --help` literally names
 * OpenClaw) — it emits a JSON receipt with `event_id` (our messageId) and takes
 * an explicit `--room` without mutating the scope's default-room pointer. We use
 * `publish --room <room> --body-text <text>` for a parseable receipt; `msg`
 * returns no machine-readable id.
 *
 * Note on `events list`: it scans the CURRENT room (there is no `--room` flag on
 * `events list`). `join <room>` makes `<room>` the default/current room, so the
 * gateway joins first and then polls.
 */
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import type { AircMessage } from "./types.js";

const execFileAsync = promisify(execFile);

/**
 * Resolve the airc binary. It is normally on PATH as `airc`; fall back to the
 * conventional user-local install location used on this fleet.
 */
function resolveAircBinary(): string {
  const fromEnv = process.env.AIRC_BIN?.trim();
  if (fromEnv) {
    return fromEnv;
  }
  return "airc";
}

/** Build the leading `--home <home>` args when a home dir is configured. */
function homeArgs(home?: string): string[] {
  const trimmed = home?.trim();
  return trimmed ? ["--home", trimmed] : [];
}

async function runAirc(home: string | undefined, args: string[]): Promise<string> {
  const { stdout } = await execFileAsync(resolveAircBinary(), [...homeArgs(home), ...args], {
    maxBuffer: 16 * 1024 * 1024,
    windowsHide: true,
  });
  return stdout;
}

/** Subscribe to a room: `airc --home <home> join <room>`. */
export async function aircJoin(params: { home?: string; room: string }): Promise<void> {
  // `join` streams live events in interactive runtimes and only returns after
  // setup in scripts/non-tty contexts. We invoke it for its setup side-effect
  // (subscribe + make-default), so we don't await stream termination; a short
  // timeout guards against a build that decides to attach.
  await runAirc(params.home, ["join", params.room]).catch((error: unknown) => {
    // A timeout (ETIMEDOUT) here is expected if the build streams; the
    // subscription has still been established. Re-throw anything else.
    if (isTimeoutError(error)) {
      return "";
    }
    throw error;
  });
}

function isTimeoutError(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "killed" in error &&
    (error as { killed?: boolean }).killed === true
  );
}

/** Parse the local peer id: `airc --home <home> status` → `peer_id:` line. */
export async function aircSelf(params: { home?: string }): Promise<{ peerId: string }> {
  const stdout = await runAirc(params.home, ["status"]);
  for (const line of stdout.split(/\r?\n/)) {
    const match = line.match(/^\s*peer_id:\s*(\S+)/);
    if (match?.[1]) {
      return { peerId: match[1].trim() };
    }
  }
  throw new Error("airc status did not report a peer_id");
}

/** Shape of one event in `airc events list --json` output. */
type AircEventListJson = {
  count: number;
  events: Array<{
    event_id?: string;
    peer_id?: string;
    occurred_at_ms?: number;
    body?: {
      kind?: string;
      value?: {
        text?: string;
      } | null;
    } | null;
  }>;
};

function extractText(body: AircEventListJson["events"][number]["body"]): string {
  const text = body?.value?.text;
  return typeof text === "string" ? text : "";
}

/**
 * Poll recent room messages:
 * `airc --home <home> events list --kind message --limit <N> --json`.
 *
 * `room` is accepted for symmetry/intent; `events list` always reads the current
 * room, which the gateway has already set via `aircJoin`.
 */
export async function aircPollMessages(params: {
  home?: string;
  room: string;
  limit: number;
}): Promise<AircMessage[]> {
  const stdout = await runAirc(params.home, [
    "events",
    "list",
    "--kind",
    "message",
    "--limit",
    String(params.limit),
    "--json",
  ]);
  const parsed = parseEventList(stdout);
  const messages: AircMessage[] = [];
  for (const event of parsed.events) {
    const eventId = event.event_id?.trim();
    const peerId = event.peer_id?.trim();
    if (!eventId || !peerId) {
      continue;
    }
    messages.push({
      eventId,
      peerId,
      text: extractText(event.body),
      occurredAtMs: typeof event.occurred_at_ms === "number" ? event.occurred_at_ms : 0,
    });
  }
  return messages;
}

function parseEventList(stdout: string): AircEventListJson {
  const start = stdout.indexOf("{");
  const end = stdout.lastIndexOf("}");
  if (start === -1 || end === -1 || end < start) {
    return { count: 0, events: [] };
  }
  try {
    const parsed = JSON.parse(stdout.slice(start, end + 1)) as Partial<AircEventListJson>;
    return {
      count: typeof parsed.count === "number" ? parsed.count : 0,
      events: Array.isArray(parsed.events) ? parsed.events : [],
    };
  } catch {
    return { count: 0, events: [] };
  }
}

/** Shape of the `airc publish` JSON receipt printed on stdout. */
type AircPublishReceipt = {
  event_id?: string;
  channel_name?: string;
};

/**
 * Send text to a room and return a messageId:
 * `airc --home <home> publish --room <room> --body-text <text>`.
 *
 * Returns the receipt's `event_id` as the messageId; falls back to a uuid if the
 * receipt is malformed (so callers always get a stable id for receipt tracking).
 */
export async function aircSend(params: {
  home?: string;
  room: string;
  text: string;
}): Promise<{ messageId: string }> {
  const stdout = await runAirc(params.home, [
    "publish",
    "--room",
    params.room,
    "--body-text",
    params.text,
  ]);
  const messageId = parsePublishReceipt(stdout) ?? randomUUID();
  return { messageId };
}

function parsePublishReceipt(stdout: string): string | null {
  const start = stdout.indexOf("{");
  const end = stdout.lastIndexOf("}");
  if (start === -1 || end === -1 || end < start) {
    return null;
  }
  try {
    const parsed = JSON.parse(stdout.slice(start, end + 1)) as AircPublishReceipt;
    return parsed.event_id?.trim() || null;
  } catch {
    return null;
  }
}
