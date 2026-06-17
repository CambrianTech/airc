/**
 * Maps airc senders and rooms onto the shared channel ingress
 * allowlist/command authorization contract.
 *
 * airc has only group rooms, so every message is a group conversation keyed by
 * room name, with the sender identified by peer id.
 */
import {
  resolveStableChannelMessageIngress,
  type StableChannelIngressIdentityParams,
} from "openclaw/plugin-sdk/channel-ingress-runtime";
import type { OpenClawConfig } from "openclaw/plugin-sdk/config-contracts";
import { getAircRuntime } from "./runtime.js";
import type { AircMessage, CoreConfig, ResolvedAircAccount } from "./types.js";

const CHANNEL_ID = "airc" as const;

function normalizeAircPeerId(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }
  return trimmed.replace(/^airc:/i, "").trim() || null;
}

const aircIngressIdentity = {
  key: "peer-id",
  normalizeEntry: normalizeAircPeerId,
  normalizeSubject: normalizeAircPeerId,
  isWildcardEntry: (entry) => normalizeAircPeerId(entry) === "*",
  entryIdPrefix: "airc-peer",
} satisfies StableChannelIngressIdentityParams;

/** Dispatch and command authorization decision for one inbound airc message. */
export type AircInboundAccess = {
  shouldDispatch: boolean;
  commandAuthorized: boolean;
};

/**
 * Resolves whether an airc message should enter the agent pipeline and whether
 * its command-style body may run tools.
 */
export async function resolveAircInboundAccess(params: {
  account: ResolvedAircAccount;
  config: CoreConfig;
  message: AircMessage;
  room: string;
}): Promise<AircInboundAccess> {
  const runtime = getAircRuntime();
  const cfg = params.config as OpenClawConfig;
  const shouldCheckCommand = runtime.channel.commands.shouldComputeCommandAuthorized(
    params.message.text,
    cfg,
  );
  const resolved = await resolveStableChannelMessageIngress({
    channelId: CHANNEL_ID,
    accountId: params.account.accountId,
    identity: aircIngressIdentity,
    cfg,
    subject: { stableId: params.message.peerId },
    conversation: {
      kind: "group",
      id: params.room,
    },
    allowFrom: params.account.allowFrom,
    dmPolicy: "allowlist",
    groupPolicy: "allowlist",
    command: shouldCheckCommand
      ? {
          cfg,
          modeWhenAccessGroupsOff: "configured",
        }
      : false,
  });

  return {
    shouldDispatch: resolved.ingress.admission === "dispatch",
    commandAuthorized: resolved.commandAccess.requested
      ? resolved.commandAccess.authorized
      : resolved.senderAccess.allowed,
  };
}
