/**
 * Converts authorized airc room messages into OpenClaw agent/model replies and
 * routes resulting outbound text back to the airc room.
 */
import type { OpenClawConfig } from "openclaw/plugin-sdk/config-contracts";
import { resolveAircInboundAccess, type AircInboundAccess } from "./access.js";
import { sendAircText } from "./outbound.js";
import { getAircRuntime } from "./runtime.js";
import { buildAircTarget } from "./target.js";
import type { AircMessage, CoreConfig, ResolvedAircAccount } from "./types.js";

const CHANNEL_ID = "airc" as const;

function resolveAccountAgentRoute(params: {
  cfg: OpenClawConfig;
  account: ResolvedAircAccount;
  target: string;
}) {
  const runtime = getAircRuntime();
  const route = runtime.channel.routing.resolveAgentRoute({
    cfg: params.cfg,
    channel: CHANNEL_ID,
    accountId: params.account.accountId,
    peer: {
      kind: "channel",
      id: params.target,
    },
  });
  const agentId = params.account.agentId ?? route.agentId;
  if (agentId === route.agentId) {
    return route;
  }
  return {
    ...route,
    agentId,
    sessionKey: runtime.channel.routing.buildAgentSessionKey({
      agentId,
      channel: CHANNEL_ID,
      accountId: params.account.accountId,
      peer: {
        kind: "channel",
        id: params.target,
      },
    }),
  };
}

async function dispatchModelReply(params: {
  account: ResolvedAircAccount;
  cfg: OpenClawConfig;
  message: AircMessage;
  route: { agentId: string };
  target: string;
}) {
  const runtime = getAircRuntime();
  const result = await runtime.llm.complete({
    agentId: params.route.agentId,
    model: params.account.model,
    maxTokens: 96,
    purpose: "airc bot reply",
    systemPrompt: params.account.systemPrompt,
    messages: [
      {
        role: "user",
        content: params.message.text,
      },
    ],
  });
  const text = result.text.trim();
  if (!text) {
    return;
  }
  await sendAircText({
    cfg: params.cfg as CoreConfig,
    accountId: params.account.accountId,
    to: params.target,
    text,
  });
}

/**
 * Dispatches one already-fetched airc room message through the configured reply
 * mode for its account.
 */
export async function handleAircInbound(params: {
  account: ResolvedAircAccount;
  config: CoreConfig;
  message: AircMessage;
  room: string;
  access?: AircInboundAccess;
}) {
  const runtime = getAircRuntime();
  const message = params.message;
  const access =
    params.access ??
    (await resolveAircInboundAccess({
      account: params.account,
      config: params.config,
      message,
      room: params.room,
    }));
  if (!access.shouldDispatch) {
    return;
  }
  const target = buildAircTarget({ chatType: "group", kind: "room", id: params.room });
  const route = resolveAccountAgentRoute({
    cfg: params.config as OpenClawConfig,
    account: params.account,
    target,
  });
  if (params.account.replyMode === "model") {
    await dispatchModelReply({
      account: params.account,
      cfg: params.config as OpenClawConfig,
      message,
      route,
      target,
    });
    return;
  }
  const senderName = message.peerId;
  const previousTimestamp = runtime.channel.session.readSessionUpdatedAt({
    storePath: runtime.channel.session.resolveStorePath(params.config.session?.store, {
      agentId: route.agentId,
    }),
    sessionKey: route.sessionKey,
  });
  // Preserve both normalized channel fields and airc-native ids so reply
  // routing, session recovery, and command authorization see the same message.
  const body = runtime.channel.reply.formatAgentEnvelope({
    channel: "airc",
    from: senderName,
    timestamp: new Date(message.occurredAtMs),
    previousTimestamp,
    envelope: runtime.channel.reply.resolveEnvelopeFormatOptions(params.config as OpenClawConfig),
    body: message.text,
  });
  const storePath = runtime.channel.session.resolveStorePath(params.config.session?.store, {
    agentId: route.agentId,
  });
  const ctxPayload = runtime.channel.reply.finalizeInboundContext({
    Body: body,
    BodyForAgent: message.text,
    RawBody: message.text,
    CommandBody: message.text,
    From: target,
    To: target,
    SessionKey: route.sessionKey,
    AccountId: route.accountId ?? params.account.accountId,
    ChatType: "group",
    WasMentioned: true,
    ConversationLabel: params.room,
    GroupChannel: params.room,
    NativeChannelId: params.room,
    SenderName: senderName,
    SenderId: message.peerId,
    Provider: CHANNEL_ID,
    Surface: CHANNEL_ID,
    MessageSid: message.eventId,
    MessageSidFull: message.eventId,
    ReplyToId: message.eventId,
    Timestamp: new Date(message.occurredAtMs).toISOString(),
    OriginatingChannel: CHANNEL_ID,
    OriginatingTo: target,
    CommandAuthorized: access.commandAuthorized,
  });
  await runtime.channel.inbound.dispatchReply({
    cfg: params.config as OpenClawConfig,
    channel: CHANNEL_ID,
    accountId: params.account.accountId,
    agentId: route.agentId,
    routeSessionKey: route.sessionKey,
    storePath,
    ctxPayload,
    recordInboundSession: runtime.channel.session.recordInboundSession,
    dispatchReplyWithBufferedBlockDispatcher:
      runtime.channel.reply.dispatchReplyWithBufferedBlockDispatcher,
    delivery: {
      deliver: async (payload) => {
        const text =
          payload && typeof payload === "object" && "text" in payload
            ? ((payload as { text?: string }).text ?? "")
            : "";
        if (!text.trim()) {
          return;
        }
        await sendAircText({
          cfg: params.config,
          accountId: params.account.accountId,
          to: target,
          text,
        });
      },
      onError: (error) => {
        throw error instanceof Error ? error : new Error(`airc dispatch failed: ${String(error)}`);
      },
    },
    replyPipeline: {},
    record: {
      onRecordError: (error) => {
        throw error instanceof Error
          ? error
          : new Error(`airc session record failed: ${String(error)}`);
      },
    },
  });
}
