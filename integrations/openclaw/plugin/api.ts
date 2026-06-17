/**
 * Public airc runtime API barrel used by plugin tests, docs, and integration
 * code that should not reach into src internals.
 */
export {
  DEFAULT_ACCOUNT_ID,
  listAircAccountIds,
  listEnabledAircAccounts,
  resolveAircAccount,
  resolveDefaultAircAccountId,
} from "./src/accounts.js";
export { aircPlugin } from "./src/channel.js";
export { aircConfigSchema } from "./src/config-schema.js";
export { aircJoin, aircPollMessages, aircSelf, aircSend } from "./src/airc-cli.js";
export { getAircRuntime, setAircRuntime } from "./src/runtime.js";
export { buildAircTarget, parseAircTarget } from "./src/target.js";
export type {
  AircAccountConfig,
  AircMessage,
  AircTarget,
  CoreConfig,
  ResolvedAircAccount,
} from "./src/types.js";
