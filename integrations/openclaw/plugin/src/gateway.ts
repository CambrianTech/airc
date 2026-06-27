/**
 * Gateway loop for the airc channel: subscribe to the room, learn our own peer
 * id, then POLL the room transcript for new messages and dispatch them into
 * OpenClaw.
 *
 * airc has no realtime websocket, so we poll `events list` on a cadence instead
 * of opening a socket (the one structural difference from a websocket-backed
 * channel like clickclack). Dedup is by event id; the first poll only seeds the
 * seen-set so a fresh gateway session never replays historical backlog.
 */
import type { ChannelGatewayContext } from "openclaw/plugin-sdk/channel-contract";
import { resolveAircInboundAccess } from "./access.js";
import { resolveAircAccount } from "./accounts.js";
import { aircJoin, aircPollMessages, aircSelf } from "./airc-cli.js";
import { handleAircInbound } from "./inbound.js";
import type { AircMessage, CoreConfig, ResolvedAircAccount } from "./types.js";

const POLL_LIMIT = 50;

async function processMessage(params: {
  account: ResolvedAircAccount;
  config: CoreConfig;
  message: AircMessage;
  room: string;
  selfPeerId: string;
}) {
  // Never react to our own messages — the agent must not loop on its replies.
  if (params.message.peerId === params.selfPeerId) {
    return;
  }
  if (!params.message.text.trim()) {
    return;
  }
  const access = await resolveAircInboundAccess({
    account: params.account,
    config: params.config,
    message: params.message,
    room: params.room,
  });
  if (!access.shouldDispatch) {
    return;
  }
  await handleAircInbound({
    account: params.account,
    config: params.config,
    message: params.message,
    room: params.room,
    access,
  });
}

/** Sleep for `ms`, resolving early if the gateway is aborted. */
async function sleepOrAbort(ms: number, abortSignal: AbortSignal): Promise<void> {
  if (abortSignal.aborted) {
    return;
  }
  await new Promise<void>((resolve) => {
    const timer = setTimeout(() => {
      abortSignal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    abortSignal.addEventListener("abort", onAbort, { once: true });
  });
}

export async function startAircGatewayAccount(ctx: ChannelGatewayContext<ResolvedAircAccount>) {
  const account = resolveAircAccount({
    cfg: ctx.cfg as CoreConfig,
    accountId: ctx.account.accountId,
  });
  if (!account.configured) {
    throw new Error(`airc is not configured for account "${account.accountId}"`);
  }
  const config = ctx.cfg as CoreConfig;
  const room = account.room;

  // 1. Subscribe to the room and learn our own peer id.
  await aircJoin({ home: account.home, room });
  const self = await aircSelf({ home: account.home });

  ctx.setStatus({
    accountId: account.accountId,
    running: true,
    configured: true,
    enabled: account.enabled,
  });

  const seen = new Set<string>();
  let initialized = false;

  // 2. Poll loop.
  while (!ctx.abortSignal.aborted) {
    let messages: AircMessage[] = [];
    try {
      messages = await aircPollMessages({ home: account.home, room, limit: POLL_LIMIT });
    } catch (error) {
      ctx.log?.warn?.(
        `[${account.accountId}] airc poll failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    }

    if (!initialized) {
      // First pass seeds the seen-set without dispatching historical backlog.
      for (const message of messages) {
        seen.add(message.eventId);
      }
      initialized = true;
    } else {
      for (const message of messages) {
        if (seen.has(message.eventId)) {
          continue;
        }
        seen.add(message.eventId);
        await processMessage({
          account,
          config,
          message,
          room,
          selfPeerId: self.peerId,
        });
      }
    }

    await sleepOrAbort(account.pollMs, ctx.abortSignal);
  }

  ctx.setStatus({ accountId: account.accountId, running: false });
}
