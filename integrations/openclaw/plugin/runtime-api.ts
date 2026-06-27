/**
 * Public runtime injection surface used by the bundled airc entry.
 */
export {
  type AircAccountConfig,
  type AircMessage,
  type AircTarget,
  type ResolvedAircAccount,
  aircJoin,
  aircPollMessages,
  aircSelf,
  aircSend,
  parseAircTarget,
  resolveAircAccount,
  setAircRuntime,
} from "./api.js";
