/**
 * Resolves airc account configuration from root channel config and named
 * account overrides.
 *
 * airc has no bearer token — auth is the local scope (home dir + identity), so
 * there is no secret-provider/token resolution here. An account is `configured`
 * once it has a `room` (defaulted to `general`); `home` is optional (when unset
 * airc resolves its own default scope).
 */
import { createAccountListHelpers } from "openclaw/plugin-sdk/account-helpers";
import { DEFAULT_ACCOUNT_ID, normalizeAccountId } from "openclaw/plugin-sdk/account-id";
import { resolveMergedAccountConfig } from "openclaw/plugin-sdk/account-resolution";
import { resolveIntegerOption } from "openclaw/plugin-sdk/number-runtime";
import { normalizeOptionalString } from "openclaw/plugin-sdk/string-coerce-runtime";
import type { AircAccountConfig, CoreConfig, ResolvedAircAccount } from "./types.js";

const DEFAULT_ROOM = "general";
const DEFAULT_POLL_MS = 2_000;
const MIN_POLL_MS = 500;
const MAX_POLL_MS = 60_000;

const { listAccountIds: listAircAccountIds, resolveDefaultAccountId: resolveDefaultAircAccountId } =
  createAccountListHelpers("airc", {
    normalizeAccountId,
    // airc needs no credentials, so the top-level config is enough to imply a
    // default account as soon as the channel block exists.
    hasImplicitDefaultAccount: (cfg) => Boolean(cfg.channels?.airc),
  });

export { DEFAULT_ACCOUNT_ID, listAircAccountIds, resolveDefaultAircAccountId };

function resolveMergedAircAccountConfig(cfg: CoreConfig, accountId: string): AircAccountConfig {
  return resolveMergedAccountConfig<AircAccountConfig>({
    channelConfig: cfg.channels?.airc as AircAccountConfig | undefined,
    accounts: cfg.channels?.airc?.accounts,
    accountId,
    omitKeys: ["defaultAccount"],
    normalizeAccountId,
  });
}

/**
 * Builds the normalized account snapshot used by gateway, outbound delivery,
 * status reporting, and channel routing.
 */
export function resolveAircAccount(params: {
  cfg: CoreConfig;
  accountId?: string | null;
}): ResolvedAircAccount {
  const accountId = normalizeAccountId(params.accountId);
  const merged = resolveMergedAircAccountConfig(params.cfg, accountId);
  const baseEnabled = params.cfg.channels?.airc?.enabled !== false;
  const enabled = baseEnabled && merged.enabled !== false;
  const home = normalizeOptionalString(merged.home);
  const room = merged.room?.trim() || DEFAULT_ROOM;
  return {
    accountId,
    enabled,
    // No token to require: a room (always defaulted) is enough to bridge. `home`
    // is optional — undefined means "let airc resolve its default scope".
    configured: Boolean(room),
    name: normalizeOptionalString(merged.name),
    home,
    room,
    agentId: normalizeOptionalString(merged.agentId),
    replyMode: merged.replyMode === "model" ? "model" : "agent",
    model: normalizeOptionalString(merged.model),
    systemPrompt: normalizeOptionalString(merged.systemPrompt),
    timeoutSeconds: merged.timeoutSeconds,
    toolsAllow: merged.toolsAllow,
    allowFrom: merged.allowFrom ?? ["*"],
    defaultTo: `airc:${room}`,
    pollMs: resolveIntegerOption(merged.pollMs, DEFAULT_POLL_MS, {
      min: MIN_POLL_MS,
      max: MAX_POLL_MS,
    }),
    config: {
      ...merged,
      allowFrom: merged.allowFrom ?? ["*"],
    },
  };
}

/**
 * Returns all enabled accounts, including the implicit default account when
 * top-level airc config is present.
 */
export function listEnabledAircAccounts(cfg: CoreConfig): ResolvedAircAccount[] {
  return listAircAccountIds(cfg)
    .map((accountId) => resolveAircAccount({ cfg, accountId }))
    .filter((account) => account.enabled);
}
