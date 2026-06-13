# WebRTC Integration

AIRC can coordinate WebRTC sessions without becoming a media
application. The substrate owns identity, signed room events,
subscriptions, route selection, WebRTC signaling, DataChannel control,
and the peer connection lifecycle. Consumers own rendering, media
production, speech, avatar behavior, moderation, and domain policy.

<img src="https://raw.githubusercontent.com/CambrianTech/continuum/main/docs/images/live-session-avatars.png" alt="Continuum live room with one human and AI personas represented as avatars in a shared video conversation" width="100%"/>

Continuum's live avatar room is one concrete consumer: humans and AI
personas share a room, while AIRC provides the signed channel and
WebRTC coordination needed for chat, commands, presence, replay,
DataChannel control, and audio/video track setup. Continuum decides how
to render avatars, route persona turns, page LoRAs, and interpret
cognitive state.

The same shape applies to other consumers:

- OpenClaw can present AIRC rooms as user-facing chats and calls.
- Hermes can orchestrate tools and agents through typed command events.
- Slack, Discord, Teams, or IRC bridges can map external channels into
  signed AIRC rooms.
- Games, VR, and live collaboration tools can use the same signaling
  and control plane while keeping media/rendering logic outside AIRC.

## Boundary

AIRC should carry:

- room membership and presence
- WebRTC offer/answer and ICE signaling
- DataChannel command/control events
- media-track metadata and lifecycle
- replayable chat and typed coordination events
- route and health state

AIRC should not carry:

- raw video frames in transcript bodies
- product-specific avatar logic
- model/LoRA routing policy
- UI layout state that belongs to a consumer
- bridge-specific command semantics

## References

- [WebRTC media tracks plan](../../docs/architecture/WEBRTC-MEDIA-TRACKS-PLAN.md)
- [Continuum integration](../continuum/README.md)
- [Generic integration boundary](../generic/README.md)
