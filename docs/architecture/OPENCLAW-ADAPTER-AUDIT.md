# OpenClaw → AIRC adapter audit (first gap pass)

Closes work card **54e1dbd0-4961-4807-9363-b4693a1532e5** (P1):
"OpenClaw adapter audit: map channels, presence, and threads onto
AIRC rooms".

This is a Phase-5 consumer proof — first gap list, not a full
adapter implementation. Scope per the card:

- room/channel mapping
- user/presence source
- event ingestion point
- smallest fixture proof
- thin adapter boundary (no AIRC-specific behavior in OpenClaw
  core)

Repo audited: openclaw at `/Users/joelteply/Development/opensource/openclaw`
(commit at audit time observable via `git -C <path> rev-parse
HEAD`). All path refs below use openclaw repo-root style per its
own `AGENTS.md`.

## What OpenClaw is

Plugin-based chat agent runtime. The architecture invariant
(from openclaw's root `AGENTS.md`): **core stays
plugin-agnostic; plugins cross into core only via
`openclaw/plugin-sdk/*`**. Concretely:

- `src/channels/*` — channel abstractions (a "channel" in
  OpenClaw terminology is a *chat platform*: Slack, Telegram,
  Discord, ...)
- `extensions/*` — one extension per channel plugin
  (`extensions/slack`, `extensions/telegram`, etc.)
- `src/gateway/*` — websocket/HTTP gateway that the UI and
  external clients connect to; presence + state versioning live
  here.
- `src/agents/*` — agent runtime (Anthropic, Vertex, etc.).

The OpenClaw "channel" concept is **not** an AIRC-style chat
room — it's a *channel family* (chat platform). Within a
channel, individual conversations are identified by
`conversationId`/`parentConversationId` and tagged with
`ChatType = "direct" | "group" | "channel"`
(`src/channels/chat-type.ts`).

## Mapping

| OpenClaw concept | OpenClaw type / path | AIRC concept | Notes |
|---|---|---|---|
| Channel family (Slack, Telegram, …) | `ChannelId` in `src/channels/plugins/channel-id.types.ts` | `ExternalIdentitySource` (per AIRC PR #985) | Closed-set enum: `Slack`, `GoogleChat`, `Discord`, `MicrosoftTeams`, plus `Other(String)` for OpenClaw-specific channels (telegram, signal, etc.) |
| Conversation within a channel | `conversationId` (string), see `src/channels/plugins/types.core.ts` | `RoomId` (`airc-core::RoomId`) | Adapter derives a stable `RoomId` from `(channel_family, conversation_id)` — same shape AIRC already uses with `Room::from_name` UUIDv5 derivation |
| Thread within a conversation | `MessageThreadId` / `parentConversationId` (`src/channels/plugins/types.core.ts:456,380`) | `reply_to: Option<EventId>` on the AIRC envelope | AIRC's existing reply chain matches OpenClaw's thread hierarchy |
| Chat type (direct/group/channel) | `ChatType` (`src/channels/chat-type.ts`) | Header `airc.openclaw.chat_type` | Filterable without decoding body, same pattern as `airc.bridge.source` |
| Channel-platform user | OpenClaw `account-summary.ts` / sender metadata | `ExternalIdentity { source, handle, display_name }` (per AIRC PR #985) | Wire frame signed by the bridge's `PeerId`; the body carries the external user — same attribution model the AIRC bridge contract already ships |
| Message body | OpenClaw message payload | `Body::Json` carrying OpenClaw's normalized message envelope | The adapter does not flatten — round-trips OpenClaw's shape inside a typed JSON body |
| Presence snapshot | `src/gateway/server/presence-events.ts:broadcastPresenceSnapshot` | AIRC lifecycle events + subscribed-event stream | OpenClaw broadcasts presence on its websocket; the adapter republishes as AIRC `PeerArrived` / typed presence events filterable by `airc.openclaw.channel` header |
| Typing / draft state | `src/channels/typing*.ts`, `src/channels/draft-stream-controls.ts` | Ephemeral diagnostic / control frames | Out of scope for this first slice; carry as a follow-up card |

## Adapter boundary

Two viable shapes — both keep OpenClaw core plugin-agnostic.

### Shape A — `extensions/airc` plugin

A new OpenClaw extension under `extensions/airc/`, structurally
parallel to `extensions/slack` and `extensions/telegram`.
Translates OpenClaw runtime events into AIRC publishes via
`airc publish` (CLI) or `airc-lib` (Rust process).

Pros:
- Lives entirely outside OpenClaw core; same boundary rule that
  Slack/Telegram already observe.
- Same install / lifecycle as any other plugin.

Cons:
- OpenClaw is TypeScript/Node; linking `airc-lib` directly
  requires Node-Rust glue or a child process.
- A subprocess per event is the "shell to airc msg" pattern
  AIRC #990 explicitly removed — would need to use the
  structured `airc publish` JSON-receipt path (also #990) so
  the bridge doesn't pay subprocess overhead per message.

### Shape B — external bridge process

A standalone Rust binary that:

1. Connects to OpenClaw's gateway websocket as a client.
2. Subscribes to OpenClaw's published events (presence,
   messages, typing).
3. Re-emits them onto AIRC using `Airc::publish` (per #990)
   with `Body::Json` payloads + `airc.openclaw.*` headers.
4. Mirrors the reverse direction: subscribes to AIRC, replays
   into OpenClaw's gateway HTTP/WS for outbound chat.

Pros:
- Stateless, Rust-native, links `airc-lib` directly.
- Same shape as the bridge contract in #985 — frame signed by
  the bridge's `PeerId`, body carries `ExternalIdentity` of the
  underlying OpenClaw user.

Cons:
- Needs to be installed/run separately, not part of OpenClaw's
  plugin lifecycle.

**Recommendation:** Shape B for the proof slice. It's smaller,
uses the AIRC primitives that already exist (publish API + bridge
contract), and stays out of OpenClaw's plugin loader entirely.
Shape A becomes worthwhile once the slice proves out, since the
plugin lifecycle is a better UX than running a separate process.

## Smallest fixture proof

End-to-end fixture, no live remote services:

1. **Stand up OpenClaw gateway** in a local dev mode against an
   in-process test channel (OpenClaw has a `test-helpers/` tree
   under `test/helpers*`; reuse those harnesses).
2. **Inject a synthetic OpenClaw message** through the test
   channel: `conversationId = "test-conv"`, `chatType =
   "channel"`, sender `{ id: "user-1", display: "Test User" }`,
   body `{ text: "hello" }`.
3. **Bridge process consumes** the gateway message:
   - Derives `RoomId` from `(channel_family="openclaw-test",
     conversation_id="test-conv")` via
     `airc_lib::derive_room_id` (already public).
   - Calls `Airc::publish(PublishTarget::RoomByName(...),
     FrameKind::Event, Body::Json(<openclaw-envelope>),
     Headers { "airc.bridge.source": "other:openclaw",
     "airc.bridge.handle": "user-1",
     "airc.openclaw.channel": "openclaw-test",
     "airc.openclaw.chat_type": "channel" })`.
4. **Second AIRC peer** subscribes via
   `Airc::subscribe_bridged_messages(...)` (from #985) and
   observes the message with:
   - Frame `peer_id == bridge_peer_id` (cryptographic identity).
   - Body's `ExternalIdentity { source: Other("openclaw"),
     handle: "user-1", display_name: Some("Test User") }`.
   - Header filter `airc.openclaw.channel == "openclaw-test"`
     hits before body decode.

Pass criterion: round-trip preserves attribution (bridge signed
the frame, body claims the OpenClaw user) AND filters work
header-only.

## Gaps that block the next slice

1. **`ExternalIdentitySource` enum lacks an `OpenClaw` variant.**
   The current enum (per #985) has `Slack`, `GoogleChat`,
   `Discord`, `MicrosoftTeams`, plus `Other(String)`. Bridges
   today would use `Other("openclaw")` — works, but loses the
   closed-enum dashboarding benefit. Decision needed: either
   add `OpenClaw` as a first-class variant (it's a per-platform
   variant in spirit, even though OpenClaw aggregates *other*
   platforms), or keep it under `Other` and document the
   convention.
2. **No structured presence event type yet.** AIRC has
   lifecycle events (`PeerArrived` etc.) but no structured
   "external presence" event analogous to OpenClaw's
   `broadcastPresenceSnapshot`. A typed
   `ExternalPresenceUpdate` body, similar to `BridgedMessage`,
   would close this without forcing the bridge to invent a
   schema. Carry as a follow-up card.
3. **OpenClaw conversations don't have stable cross-process
   ids.** `conversationId` is per-platform (e.g. Slack's
   `C012ABCD`). The adapter has to normalize
   `(channel_family, conversation_id)` into a single key before
   deriving an AIRC `RoomId`, otherwise two OpenClaw
   installations talking to the same Slack workspace would land
   on different AIRC rooms. The bridge must persist its own
   mapping.
4. **No back-pressure / drop policy.** OpenClaw's
   `broadcastPresenceSnapshot` already uses `dropIfSlow: true`.
   AIRC bridges should do the same — emit a typed
   `WorkspaceLeaseViolation`-style diagnostic (per #988) when
   the AIRC publish back-pressures, rather than queueing
   unbounded.

## Non-scope (explicitly deferred)

- **Outbound from AIRC to OpenClaw.** This audit only covers
  ingestion (OpenClaw → AIRC). Reverse direction
  (AIRC-originated messages routed back into OpenClaw
  conversations) is a follow-up card.
- **Slack/Telegram-specific quirks.** Each OpenClaw channel
  plugin has its own quirks (Slack's ephemeral messages,
  Telegram's bot vs user accounts). The adapter operates at
  OpenClaw's *normalized* level, treating channel-specific
  variance as plugin business.
- **Agent tool calls / channel-message tools.** OpenClaw's
  `ChannelAgentTool` / `ChannelMessageToolDiscovery` surfaces
  belong to the agent runtime, not the chat substrate. AIRC's
  `request/reply` contract (see Hermes audit, card 9ccb3146)
  is the right place to handle those once that card lands.
- **Telephony / voice channels.** OpenClaw has voice channel
  plugins; routing those over AIRC needs the WebRTC media
  primitives (card a1bed3d8 territory), separate slice.

## Cross-references

- AIRC bridge contract: [PR #985](https://github.com/CambrianTech/airc/pull/985),
  module `airc_lib::external_identity`.
- AIRC structured publish: [PR #990](https://github.com/CambrianTech/airc/pull/990),
  module `airc_lib::publish`.
- AIRC lease-zone enforcement: [PR #988](https://github.com/CambrianTech/airc/pull/988),
  module `airc_cli::lease`.
- Consumer matrix anchor:
  [`CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md`](CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md).
- OpenClaw architecture: openclaw repo `AGENTS.md` + channel /
  gateway docs under `src/channels/AGENTS.md` and
  `src/gateway/protocol/AGENTS.md`.
