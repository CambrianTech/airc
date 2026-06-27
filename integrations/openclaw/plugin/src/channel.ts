/**
 * airc channel plugin definition: target parsing, account config, status,
 * gateway startup, and outbound delivery wiring.
 *
 * airc is a Rust mesh-chat / coordination grid with only group rooms (no DMs,
 * no threads), driven over the `airc` CLI rather than an HTTP/websocket API.
 */
import {
  buildChannelOutboundSessionRoute,
  createChatChannelPlugin,
} from "openclaw/plugin-sdk/channel-core";
import type { ChannelPlugin } from "openclaw/plugin-sdk/channel-core";
import {
  createMessageReceiptFromOutboundResults,
  defineChannelMessageAdapter,
} from "openclaw/plugin-sdk/channel-outbound";
import { getChatChannelMeta } from "openclaw/plugin-sdk/channel-plugin-common";
import {
  createComputedAccountStatusAdapter,
  createDefaultChannelRuntimeState,
} from "openclaw/plugin-sdk/status-helpers";
import {
  DEFAULT_ACCOUNT_ID,
  listAircAccountIds,
  resolveAircAccount,
  resolveDefaultAircAccountId,
} from "./accounts.js";
import { aircConfigSchema } from "./config-schema.js";
import { startAircGatewayAccount } from "./gateway.js";
import { sendAircText } from "./outbound.js";
import {
  buildAircTarget,
  looksLikeAircTarget,
  normalizeAircTarget,
  parseAircTarget,
} from "./target.js";
import type { CoreConfig, ResolvedAircAccount } from "./types.js";

const CHANNEL_ID = "airc" as const;
const meta = { ...getChatChannelMeta(CHANNEL_ID) };

const aircMessageAdapter = defineChannelMessageAdapter({
  id: CHANNEL_ID,
  durableFinal: {
    capabilities: {
      text: true,
      replyTo: true,
      thread: false,
      messageSendingHooks: true,
    },
  },
  send: {
    text: async (ctx) => {
      const result = await sendAircText({
        cfg: ctx.cfg as CoreConfig,
        accountId: ctx.accountId,
        to: ctx.to,
        text: ctx.text,
      });
      const replyToId = ctx.replyToId ?? undefined;
      return {
        messageId: result.messageId,
        receipt: createMessageReceiptFromOutboundResults({
          results: [{ channel: CHANNEL_ID, messageId: result.messageId }],
          replyToId,
          kind: "text",
        }),
      };
    },
  },
});

/**
 * Channel plugin instance registered by the bundled airc entry.
 */
export const aircPlugin: ChannelPlugin<ResolvedAircAccount> = createChatChannelPlugin({
  base: {
    id: CHANNEL_ID,
    meta,
    capabilities: {
      chatTypes: ["group"],
      threads: false,
    },
    reload: { configPrefixes: ["channels.airc"] },
    configSchema: aircConfigSchema,
    config: {
      listAccountIds: (cfg) => listAircAccountIds(cfg as CoreConfig),
      resolveAccount: (cfg, accountId) => resolveAircAccount({ cfg: cfg as CoreConfig, accountId }),
      defaultAccountId: (cfg) => resolveDefaultAircAccountId(cfg as CoreConfig),
      isConfigured: (account) => account.configured,
      resolveAllowFrom: ({ cfg, accountId }) =>
        resolveAircAccount({ cfg: cfg as CoreConfig, accountId }).allowFrom,
      resolveDefaultTo: ({ cfg, accountId }) =>
        resolveAircAccount({ cfg: cfg as CoreConfig, accountId }).defaultTo,
    },
    messaging: {
      targetPrefixes: ["airc"],
      normalizeTarget: normalizeAircTarget,
      inferTargetChatType: () => "group",
      targetResolver: {
        looksLikeId: looksLikeAircTarget,
        hint: "<airc:roomname|roomname>",
      },
      resolveOutboundSessionRoute: ({ cfg, agentId, accountId, target }) => {
        const parsed = parseAircTarget(target);
        return buildChannelOutboundSessionRoute({
          cfg,
          agentId,
          channel: CHANNEL_ID,
          accountId,
          peer: {
            kind: "channel",
            id: buildAircTarget(parsed),
          },
          chatType: "group",
          from: `airc:${accountId ?? DEFAULT_ACCOUNT_ID}`,
          to: buildAircTarget(parsed),
        });
      },
      resolveSessionConversation: ({ rawId }) => {
        const parsed = parseAircTarget(rawId);
        return {
          id: parsed.id,
          baseConversationId: parsed.id,
          parentConversationCandidates: [parsed.id],
        };
      },
    },
    status: createComputedAccountStatusAdapter<ResolvedAircAccount>({
      defaultRuntime: createDefaultChannelRuntimeState(DEFAULT_ACCOUNT_ID),
      buildChannelSummary: ({ account, snapshot }) => ({
        ok: snapshot.configured,
        label: snapshot.configured ? "configured" : "missing config",
        // `room` is an airc-specific extra not on the base snapshot type, so read
        // it from the resolved account (which `buildChannelSummary` receives).
        detail: account.room,
      }),
      resolveAccountSnapshot: ({ account }) => ({
        accountId: account.accountId,
        name: account.name,
        enabled: account.enabled,
        configured: account.configured,
        room: account.room,
      }),
    }),
    gateway: {
      startAccount: startAircGatewayAccount,
    },
    message: aircMessageAdapter,
  },
  outbound: {
    base: {
      deliveryMode: "direct",
    },
    attachedResults: {
      channel: CHANNEL_ID,
      sendText: async ({ cfg, to, text, accountId }) =>
        await sendAircText({
          cfg: cfg as CoreConfig,
          accountId,
          to,
          text,
        }),
    },
  },
});
