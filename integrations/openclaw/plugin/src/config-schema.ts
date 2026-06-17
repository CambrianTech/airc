/**
 * Zod-backed config schema for airc channel accounts.
 *
 * No secret/token field — airc auth is the local scope (home dir + identity).
 */
import { buildChannelConfigSchema } from "openclaw/plugin-sdk/channel-config-schema";
import { z } from "zod";

const AircAccountConfigSchema = z
  .object({
    name: z.string().optional(),
    enabled: z.boolean().optional(),
    home: z.string().optional(),
    room: z.string().optional(),
    agentId: z.string().optional(),
    replyMode: z.enum(["agent", "model"]).optional(),
    model: z.string().optional(),
    systemPrompt: z.string().optional(),
    timeoutSeconds: z.number().int().min(1).max(3_600).optional(),
    toolsAllow: z.array(z.string()).optional(),
    allowFrom: z.array(z.string()).optional(),
    pollMs: z.number().int().min(500).max(60_000).optional(),
  })
  .strict();

const AircConfigSchema = AircAccountConfigSchema.extend({
  accounts: z.record(z.string(), AircAccountConfigSchema.partial()).optional(),
  defaultAccount: z.string().optional(),
}).strict();

/**
 * Config schema exported to core so `openclaw doctor` and config validation
 * understand both default and named airc accounts.
 *
 * The explicit `ReturnType` annotation keeps the exported type nameable: the
 * SDK's `ChannelConfigSchema` type lives in an internal chunk and isn't exported
 * on a public path, so without this annotation `tsc` reports TS2883 (inferred
 * type cannot be named portably).
 */
export const aircConfigSchema: ReturnType<typeof buildChannelConfigSchema> =
  buildChannelConfigSchema(AircConfigSchema);
