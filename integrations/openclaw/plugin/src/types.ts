/**
 * Shared airc config, runtime account, message, and target types.
 *
 * airc is a Rust mesh-chat / coordination grid. Unlike a hosted chat backend,
 * it has no HTTP API, no bearer token, and no workspaces — auth is the local
 * scope (a `home` state directory plus the persisted identity inside it). It
 * also has only group rooms: no DMs, no threads.
 */
import type { OpenClawConfig } from "openclaw/plugin-sdk/config-contracts";

/** User-configurable settings for one airc account (scope). */
export type AircAccountConfig = {
  name?: string;
  enabled?: boolean;
  /**
   * State directory for the persisted airc identity + IPC socket (the `--home`
   * flag). When unset, airc resolves its own default scope (the git project
   * root's `.airc`, or `$AIRC_HOME`).
   */
  home?: string;
  /** Room (channel) name to bridge. Defaults to `general`. */
  room?: string;
  agentId?: string;
  replyMode?: "agent" | "model";
  model?: string;
  systemPrompt?: string;
  timeoutSeconds?: number;
  toolsAllow?: string[];
  allowFrom?: string[];
  /** Poll interval (ms) between `events list` scans. Defaults to 2000, min 500. */
  pollMs?: number;
};

/** Root airc channel config with optional named accounts. */
export type AircConfig = AircAccountConfig & {
  accounts?: Record<string, Partial<AircAccountConfig>>;
  defaultAccount?: string;
};

/** OpenClaw config narrowed to include airc channel settings. */
export type CoreConfig = OpenClawConfig & {
  channels?: OpenClawConfig["channels"] & {
    airc?: AircConfig;
  };
};

/** Normalized account snapshot consumed by runtime paths. */
export type ResolvedAircAccount = {
  accountId: string;
  enabled: boolean;
  configured: boolean;
  name?: string;
  /** Resolved `--home`, or undefined to let airc pick its default scope. */
  home?: string;
  room: string;
  agentId?: string;
  replyMode: "agent" | "model";
  model?: string;
  systemPrompt?: string;
  timeoutSeconds?: number;
  toolsAllow?: string[];
  allowFrom: string[];
  defaultTo: string;
  pollMs: number;
  config: AircAccountConfig;
};

/**
 * One airc room message, normalized from an `events list --kind message --json`
 * event. `eventId` is the substrate event id (used for dedup + ReplyToId),
 * `peerId` identifies the sender (so we can skip our OWN messages).
 */
export type AircMessage = {
  eventId: string;
  peerId: string;
  text: string;
  occurredAtMs: number;
};

/** Parsed outbound destination for airc delivery (rooms only). */
export type AircTarget = { chatType: "group"; kind: "room"; id: string };
