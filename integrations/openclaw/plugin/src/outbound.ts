/**
 * Outbound airc delivery helper. airc has only group rooms, so delivery is a
 * single `publish` to the target room via the airc CLI bridge.
 */
import { resolveAircAccount } from "./accounts.js";
import { aircSend } from "./airc-cli.js";
import { parseAircTarget } from "./target.js";
import type { CoreConfig } from "./types.js";

/**
 * Sends text to a normalized airc room target and returns the created event id
 * for receipt/session tracking.
 */
export async function sendAircText(params: {
  cfg: CoreConfig;
  accountId?: string | null;
  to: string;
  text: string;
}): Promise<{ to: string; messageId: string }> {
  const account = resolveAircAccount({ cfg: params.cfg, accountId: params.accountId });
  const parsed = parseAircTarget(params.to);
  const result = await aircSend({ home: account.home, room: parsed.id, text: params.text });
  return { to: params.to, messageId: result.messageId };
}
