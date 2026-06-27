/**
 * Parser and formatter for airc outbound target strings.
 *
 * airc has only group rooms (no DMs, no threads), so a target is just a room
 * name, optionally prefixed with `airc:`.
 */
import type { AircTarget } from "./types.js";

/** Parses `airc:roomname` or a bare `roomname` into a room target. */
export function parseAircTarget(raw: string): AircTarget {
  const value = raw.trim();
  if (!value) {
    throw new Error("airc target is required");
  }
  const withoutPrefix = value.replace(/^airc:/i, "").trim();
  if (!withoutPrefix) {
    throw new Error(`Unsupported airc target: ${raw}`);
  }
  return { chatType: "group", kind: "room", id: withoutPrefix };
}

/** Formats a parsed airc target back into canonical target syntax. */
export function buildAircTarget(target: AircTarget): string {
  return `${target.kind}:${target.id}`;
}

/** Normalizes user-entered airc target text for channel routing. */
export function normalizeAircTarget(raw: string): string {
  return buildAircTarget(parseAircTarget(raw));
}

/** Reports whether a target string can be offered to the airc parser. */
export function looksLikeAircTarget(raw: string): boolean {
  return /^airc:/i.test(raw.trim()) || raw.trim().length > 0;
}
