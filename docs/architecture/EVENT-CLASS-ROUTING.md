# Typed event classes and routing

Status: design and implementation under review in AIRC #1629. Not deployed.
Implementation baseline: a28d2530. Full validation and durable adoption remain open.

## Contract

The producer supplies event semantics as a required `airc_core::EventClass`.
The signature binds that class together with the envelope. A receiver validates
transport-kind/class consistency before admission. Missing or unknown classes
are errors. Payload text, JSON fields, names, mention prefixes and display glyphs
never determine the destination or subscription.

`EventClass` is a closed enum with nested typed detail:

- `Correspondence(Message | Announcement)`: speech intended for another reader.
- `Activity(Intent | WorkReceipt | WorkState | WorkPresence | Coordination | Presence)`:
  observations, work obligations, coordination and presence are distinct variants.
- `Control(Command | CommandResult | Session | Receipt | Addressed | Signal | Stream |
  Lifecycle(LifecycleKind))`: protocol and substrate operations.

Lifecycle detail distinguishes peer arrival/departure, wire establishment/loss,
room join/part, subscription advancement, and identity/doctrine/wall/channel-purpose
publication. Each variant lives in `crates/airc-core/src/event_class.rs`; adding one
requires updating exhaustive protocol mappings and tests, not a string convention.
Delivery durability, target peer/room and event semantics are separate dimensions.
A class is not authorization: signature, trust and addressed verification still apply.

## One predicate across storage and delivery

`EventSelection` selects a family, exact variant, union, or all events.
`HistoryMatch` combines that selection with sender, self-echo, header and addressee
constraints. Memory, SQL, durable bus replay and SDK consumers apply the same
semantics. Filtering precedes page limits and counts: excluded activity cannot
consume a correspondence page. SQL encodings are persistence details, not public
routing strings. Live delivery and replay must select the same events.

`EventClass::transcript_kind()` owns the transcript projection. The transport's
coarse frame category cannot reconstruct semantic subtype. Addressed traffic is
refused transcript conversion and enters its verified receiver instead.

The common agent feed selects Correspondence plus exact WorkState and Coordination.
Conversation history selects Correspondence. Work projection selects WorkState and
WorkPresence and decodes its typed payload only after selection. Receipt, stream,
presence and tool-observation traffic do not become conversational notifications.
A malformed selected payload produces a typed error; it cannot fall through into
another class. Durable replay errors terminate the subscription explicitly before
later ring/live events, preserving the consumer's recovery obligation.

## Producer integration

AIRC work producers exhaustively assign WorkState, WorkPresence or Coordination.
Continuum must assign tool observations to WorkReceipt and intentional speech to
Correspondence. Its persona subscriptions must consume the shared selection API.
This integration is still outstanding; changing only the AIRC reader is insufficient.
Bodies remain content and are decoded by their owning application after routing.

## Adoption and acceptance

There is no compatibility decoder or guessed class. Populated historical stores
without signed classification are refused before schema mutation. Read-only audit
access preserves their original records. This is a rollout blocker, not a completed
adoption strategy: durable obligations must remain available through an explicit
reviewed adoption process before promotion. Account plus requested-scope preflight
is implemented; exhaustive durable consumer inventory is not yet established.

Required evidence includes signature tampering and same-frame subtype changes;
activity floods with bounded selected pages/count parity; live/replay agreement;
malformed typed payload refusal; replay failure without cursor advancement; and
actual reciprocal consumer delivery after supported deployment. Compilation or
transport ACKs alone do not establish these outcomes. Final-head CI, history adoption
and Continuum integration must complete before this design is called deployed.
