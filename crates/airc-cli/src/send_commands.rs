//! `airc send` / `airc msg`: publish one text message to a subscribed room
//! and print the sender's receipt, with the @mention audience check.
//!
//! Moved out of `commands.rs` unchanged (a pure move: same code, same
//! tests). The receipt's reach rule changes in a follow-up, in this file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use airc_core::RoomId;
use airc_ipc::DaemonClient;
use airc_lib::{Airc, PeerSpec};

use crate::commands::{
    attached_airc, ensure_daemon_running, mesh_publish_target, reject_recovery_room,
    runtime_headers, sync_daemon_peers_for_current_rooms,
};

/// Honest one-line receipt for a completed `send`/`msg`/`publish`.
///
/// What is TRUE synchronously, by the time the publish call returns:
/// the frame is signed, persisted to the local store, fanned out to
/// in-process subscribers, written to the wire, and offered to the
/// routed-forward tap. Any scope tailing this channel on THIS machine
/// has it.
///
/// What is NOT knowable here: whether any remote peer actually
/// received it. `peer_count` is the count of cryptographically-paired
/// *enrolled* remote peers ([`Airc::peers`]) — an address book, not a
/// delivery receipt. Routed forwarding and its delivery acks are
/// drained by [`airc_lib::RoutedForwarder`] in a background loop that
/// has not run by the time the CLI returns, so there is no honest
/// synchronous forwarded/acked count to print.
///
/// Therefore the verb is "queued"/"addressed", never "sent to N
/// peers" — the latter implied confirmed delivery to the enrolled
/// count and produced false "it works" claims when zero peers had
/// actually received anything. To confirm delivery, the operator runs
/// `airc doctor --health` (route status), which reads the forwarder's
/// real ack counters.
///
/// `connected_lan_peers` (from `Status`, refreshed by the daemon's
/// route-refresh loop) is the set room broadcast can ACTUALLY reach
/// right now — the forwarder fans out only over live LAN connections.
/// When it is `0` while peers are enrolled, the send reached NO remote
/// machine, and the receipt says so LOUDLY: enrolling a peer is an
/// address-book entry, not a live route, and a silently-zero connected
/// set is exactly how a fully-broken fan-out masqueraded as a healthy
/// channel (issue #1243 — the loopback-only legibility trap).
fn format_send_receipt(
    channel_name: &str,
    channel_id: &str,
    enrolled_peers: usize,
    connected_lan_peers: usize,
) -> String {
    if enrolled_peers == 0 {
        format!(
            "queued to {channel_name} ({channel_id}) — 0 enrolled remote peer(s); \
             any scope tailing this channel on this machine will receive it."
        )
    } else if connected_lan_peers == 0 {
        // LOUD: enrolled peers exist but the daemon holds no live LAN
        // connection, so room broadcast forwarded to nobody. This is the
        // signal whose absence let a broken fan-out read as success.
        format!(
            "queued to {channel_name} ({channel_id}) — ⚠ reached 0 of {enrolled_peers} \
             enrolled remote peer(s): NONE are currently connected, so no remote machine \
             received this (run `airc doctor --health` to see why the routes are down). \
             Any scope tailing this channel on this machine still has it."
        )
    } else {
        // Report REACH, not the address book. `enrolled_peers` is every peer this
        // scope has ever enrolled — across every room, for all time — so printing
        // it beside a live count invites the reader to divide, and that ratio is
        // meaningless: the enrolled set is not this room's audience, and
        // `connected_lan_peers` counts LAN links only.
        //
        // Live 2026-08-07 that arithmetic manufactured a false alarm. "addressed 52
        // enrolled remote peer(s), 1 currently connected" reads as 2% reach; the
        // ack ledger for the same period showed 767 of 771 delivered — 99.5%. A
        // card was filed on the strength of the receipt (#340) and the receipt was
        // the only thing wrong. An instrument that reads as a catastrophe during
        // healthy operation is worse than no instrument: it burns the operator's
        // trust in every future alarm.
        //
        // So: state the live count as a plain fact with no denominator to divide
        // by, and point at `airc doctor --health`, which reads the ACK ledger
        // (#280 — delivery is a returned ack, never a connection's existence).
        let peers = if connected_lan_peers == 1 {
            "1 peer"
        } else {
            "peers"
        };
        let count = if connected_lan_peers == 1 {
            String::new()
        } else {
            format!("{connected_lan_peers} ")
        };
        format!(
            "queued to {channel_name} ({channel_id}) — live to {count}{peers} now; \
             delivery is asynchronous and confirmed by ACK, not by this line \
             (run `airc doctor --health` for the delivery ledger). \
             {enrolled_peers} peer(s) are enrolled in this scope's address book, \
             which is not this room's audience — do not read it as a reach ratio. \
             Any scope tailing this channel on this machine also receives it."
        )
    }
}

/// Presence horizon for the @mention audience check. Generous on
/// purpose: a peer who reads this room but is between heartbeats must
/// still count as audience — this warning exists to catch a peer who
/// has NEVER been seen here, not to police liveness.
const MENTION_AUDIENCE_WITHIN: Duration = Duration::from_secs(48 * 60 * 60);
/// Presence-scan window for the @mention audience check (events).
const MENTION_AUDIENCE_WINDOW: usize = 4096;

/// The leading `@name` of a message body — the human addressing
/// convention (`airc msg @peer …`). The `@` is a label, not routing:
/// delivery is still room broadcast, which is exactly why the audience
/// check below exists. Returns the bare name without `@`.
fn leading_mention(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('@')?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// #270 family, live-proven 2026-08-12: a message addressed `@peer` was
/// sent — twice, with green receipts — into a room whose channel id was
/// the sender's own operator uuid, a room the addressed peer cannot
/// hear. The receipt never said so, and the miss was diagnosed as a
/// transport failure for an hour. This check resolves the leading
/// @mention against the room's roster (durable presence + identity
/// join) and returns a LOUD warning line when nobody matching the
/// mention has ever been seen in the room. Advisory only: the send has
/// already happened, the roster is presence-derived (an offline-but-
/// subscribed peer beyond the horizon is a possible false alarm — the
/// wording says "not seen", never "not subscribed"), and any roster
/// error yields `None` so the check can never break a send.
async fn mention_audience_warning(
    airc: &Airc,
    text: &str,
    channel: RoomId,
    channel_name: &str,
) -> Option<String> {
    let mention = leading_mention(text)?;
    let roster = airc
        .room_roster_in(
            Some(channel),
            MENTION_AUDIENCE_WITHIN,
            MENTION_AUDIENCE_WINDOW,
        )
        .await
        .ok()?;
    mention_audience_verdict(mention, &roster, airc.peer_id(), channel_name)
}

/// The decision half of [`mention_audience_warning`], split out from the
/// roster fetch so the wording is testable without an `Airc`.
///
/// The strong "has not been seen" line asserts ABSENCE. That is only
/// honest when the roster could have answered the question at all — every
/// OTHER peer in the window is named, so a failed match really does mean
/// "not seen in this window". (Never "not subscribed": a peer quieter than
/// [`MENTION_AUDIENCE_WITHIN`] is absent from the roster entirely, which is
/// why the emitted string says "not seen" and hedges the rest.)
/// `RoomMember::display_name` is documented as `None` for a peer that is
/// PRESENT but has not published an identity card; a mention that matches
/// nobody may simply BE one of those peers.
///
/// #262 caught the all-unnamed case. #1378 caught the same defect one
/// level up, live on IntelMac 2026-09-04: both other grid peers were
/// reported as "will NEVER receive this" seconds after each had posted,
/// because a single OTHER named member satisfied `any()`. One named member
/// cannot license a negative about a different, unnamed one — so the guard
/// is `all`, not `any`.
///
/// Two things the #1378 review then caught in that fix (card 74e8e6af):
///
/// - **`me` is excluded from the accounting.** A roster includes `self`, so
///   an operator who has never run `airc identity set` is an unnamed member
///   of EVERY room: `unnamed > 0` would be permanently true, the strong line
///   could never fire, and the soft line would describe the operator to
///   themselves as a third party. `heard` still scans the FULL roster, so
///   mentioning your own name resolves rather than warning.
/// - **Peers are counted distinctly.** `room_roster_in` yields one row per
///   (peer, client session) — two agent tabs on one box are two rows sharing
///   a peer id — so a row count printed as "peer(s)" is a number the data
///   does not support.
fn mention_audience_verdict(
    mention: &str,
    roster: &[airc_lib::RoomMember],
    me: airc_lib::PeerId,
    channel_name: &str,
) -> Option<String> {
    let needle = mention.to_ascii_lowercase();
    let heard = roster.iter().any(|member| {
        member
            .display_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(mention))
            || member
                .peer_id
                .to_string()
                .to_ascii_lowercase()
                .starts_with(&needle)
    });
    if heard {
        return None;
    }
    // Both clauses matter and neither depends on anyone's naming state: the
    // "send in a room they read" half is what actually ended #270's hour of
    // misdiagnosis, and after the `all` guard the soft path is the COMMON
    // path in any room holding an uncarded peer.
    let hint = "Check where they speak (`airc events list --limit 5000 --kind message`) \
                and send in a room they read, e.g. `airc msg --room general ...`.";
    let others: Vec<&airc_lib::RoomMember> = roster.iter().filter(|m| m.peer_id != me).collect();
    let distinct = |members: &[&airc_lib::RoomMember]| {
        members
            .iter()
            .map(|m| m.peer_id)
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    // Two different facts, said separately: an empty roster means nobody was
    // seen INCLUDING you, so "other than yourself" would imply you were.
    if roster.is_empty() {
        return Some(format!(
            "⚠ cannot verify '@{mention}' reaches '{channel_name}': no peer at all has been \
             seen in this room within the presence window, so this mention cannot be \
             resolved either way (#262). {hint}"
        ));
    }
    if others.is_empty() {
        return Some(format!(
            "⚠ cannot verify '@{mention}' reaches '{channel_name}': no peer other than \
             yourself has been seen in this room within the presence window, so this \
             mention cannot be resolved either way (#262). {hint}"
        ));
    }
    let unnamed: Vec<&airc_lib::RoomMember> = others
        .iter()
        .copied()
        .filter(|m| m.display_name.is_none())
        .collect();
    if !unnamed.is_empty() {
        return Some(format!(
            "⚠ cannot verify '@{mention}' reaches '{channel_name}': {} of {} peer(s) seen \
             here have not published an identity card, so a name match cannot succeed \
             against them either way (#262). {hint}",
            distinct(&unnamed),
            distinct(&others)
        ));
    }
    Some(format!(
        "⚠ '@{mention}' has not been seen in '{channel_name}' (48h presence window) — \
         if they are not subscribed to this room they will NEVER receive this. {hint}"
    ))
}

/// `send` — local-fs single-shot send to the current room. Routes
/// through `Airc::say`; ad-hoc `--peer` flags are enrolled in the
/// in-process registry for the duration of the invocation.
pub async fn run_send(
    home: &Path,
    peers: Vec<PeerSpec>,
    room: Option<&str>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    reject_recovery_room(room)?;
    let airc = attached_airc(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    // Card a979e5c2 reviewer NIT — symmetric with run_msg: keep the
    // daemon's per-wire subscriber index aware of every subscribed
    // room before the send goes out. Load-bearing in particular when
    // `--room <name>` targets a room the daemon hasn't recently
    // touched. Safe no-op when the index is already fresh.
    let socket = crate::cli::default_socket_path_in(home);
    sync_daemon_peers_for_current_rooms(home, socket).await?;
    // Card a979e5c2 (seam #5): `--room <name>` routes ONE message
    // to a subscribed-but-not-current room without mutating this
    // scope's default-room pointer. Same shape as `airc publish`.
    // Without `--room`, the historical "current room + runtime
    // headers" path runs unchanged.
    let target = mesh_publish_target(&airc, room).await?;
    let receipt = airc
        .publish(
            target,
            airc_protocol::FrameKind::Message,
            airc_core::Body::text(text),
            runtime_headers()?,
        )
        .await?;
    let (channel_name, channel) = (receipt.channel_name, receipt.channel_id);
    let channel_id = channel.to_string();
    // `peers()` is the enrolled-remote-peer address book, NOT a
    // delivery count — see `format_send_receipt` for why the receipt
    // says "queued/addressed" rather than "sent to N peers".
    let peer_count = airc.peers().await?.len();
    // Ask the daemon how many of those peers it currently holds a LIVE
    // LAN connection to — the set room broadcast can actually reach.
    // A cheap Status round-trip; on any failure we fall back to 0,
    // which the receipt frames as "none currently connected" rather
    // than inventing reach the daemon can't confirm.
    let connected_lan_peers = DaemonClient::new(crate::cli::default_socket_path_in(home))
        .status()
        .await
        .map(|status| status.connected_lan_peers)
        .unwrap_or(0);
    println!(
        "{}",
        format_send_receipt(&channel_name, &channel_id, peer_count, connected_lan_peers)
    );
    if let Some(warning) = mention_audience_warning(&airc, text, channel, &channel_name).await {
        println!("{warning}");
    }
    Ok(())
}

pub async fn run_msg(
    home: &Path,
    socket: PathBuf,
    room: Option<&str>,
    text: &str,
    to: Option<airc_core::PeerId>,
) -> Result<(), Box<dyn std::error::Error>> {
    reject_recovery_room(room)?;
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    sync_daemon_peers_for_current_rooms(home, socket.clone()).await?;
    let airc = Airc::attach(home, socket).await?;
    // Card a979e5c2 (seam #5): `--room <name>` routes ONE message
    // to a subscribed-but-not-current room without mutating this
    // scope's default-room pointer. Same shape as `airc publish`.
    // Without `--room`, the historical "current room" path runs
    // unchanged.
    let target = mesh_publish_target(&airc, room).await?;
    let target = match to {
        Some(peer) => target.to_peer(peer),
        None => target,
    };
    let receipt = airc
        .publish(
            target,
            airc_protocol::FrameKind::Message,
            airc_core::Body::text(text),
            runtime_headers()?,
        )
        .await?;
    let (channel_name, channel) = (receipt.channel_name, receipt.channel_id);
    let channel_id = channel.to_string();
    if let Some(peer) = to {
        // Publishing has already succeeded locally. A name lookup cannot undo it.
        let recipient = match airc.peer_alias(peer).await {
            Ok(Some(name)) => format!("{name} ({peer})"),
            Ok(None) => peer.to_string(),
            Err(error) => format!("{peer} (name lookup failed: {error})"),
        };
        println!(
            "Queued for {recipient} in {channel_name}. Event {}. Recipient delivery is unconfirmed.",
            receipt.event_id
        );
        return Ok(());
    }
    // Same enrolled-vs-delivered honesty fix as run_send, for the
    // daemon-attached send path. `peers()` is the address book, not a
    // delivery receipt; the daemon's live-connection count (Status) is
    // the set room broadcast can actually reach.
    let peer_count = airc.peers().await?.len();
    let connected_lan_peers = DaemonClient::new(crate::cli::default_socket_path_in(home))
        .status()
        .await
        .map(|status| status.connected_lan_peers)
        .unwrap_or(0);
    println!(
        "{}",
        format_send_receipt(&channel_name, &channel_id, peer_count, connected_lan_peers)
    );
    if let Some(warning) = mention_audience_warning(&airc, text, channel, &channel_name).await {
        println!("{warning}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches (live 2026-08-12): the @mention parse feeding the
    /// deaf-room warning. `@name` at the start of a body is an addressing
    /// intent and must lift the bare name (stopping at the first
    /// non-name char); a mid-text `@`, a bare `@`, or plain prose must
    /// not — a false mention would fire the audience warning on
    /// messages that never addressed anyone.
    #[test]
    fn leading_mention_lifts_addressing_intent_only() {
        assert_eq!(
            leading_mention("@BigMama LANE SPLIT per Joel"),
            Some("BigMama")
        );
        assert_eq!(
            leading_mention("@peer-7711fe60: ping"),
            Some("peer-7711fe60")
        );
        assert_eq!(leading_mention("@M5,ack"), Some("M5"));
        assert_eq!(leading_mention("hello @BigMama"), None);
        assert_eq!(leading_mention("@ stray at"), None);
        assert_eq!(leading_mention("plain prose"), None);
        assert_eq!(leading_mention(""), None);
    }

    fn roster_member(name: Option<&str>) -> airc_lib::RoomMember {
        airc_lib::RoomMember {
            peer_id: airc_lib::PeerId(uuid::Uuid::new_v4()),
            display_name: name.map(str::to_string),
            runtime: "test".to_string(),
            availability: None,
            last_seen_ms: 0,
        }
    }

    /// what this catches (live 2026-09-04, #262 follow-up): asserting ABSENCE
    /// about a peer the roster structurally cannot resolve. Both other grid
    /// peers were told "@them will NEVER receive this" seconds after each had
    /// posted into the room, because ONE other member — the sender's own
    /// published card — satisfied the old `any(display_name.is_some())` guard.
    /// `display_name: None` means present-but-unnamed, so an unmatched mention
    /// may BE that peer; only an all-named roster can license the strong line.
    ///
    /// The mixed roster must hold a named OTHER peer, not just a named `me`:
    /// once `me` is excluded from the accounting (card 74e8e6af) an all-unnamed
    /// remainder is merely the #262 case, which was already fixed before #1378.
    /// The invariant that needs pinning is one named peer failing to license a
    /// negative about a DIFFERENT unnamed one.
    ///
    /// Mutation check: expressing the guard as `any` over either subject —
    /// `roster` or `others` — fails the mixed-roster asserts below.
    #[test]
    fn absence_is_only_asserted_when_every_present_peer_is_named() {
        let strong = "will NEVER receive";
        let me = roster_member(Some("IntelMac"));

        // Mixed roster — the live shape. A named OTHER member must NOT license
        // a negative about the unnamed one beside it.
        let mixed = vec![me.clone(), roster_member(Some("M5")), roster_member(None)];
        let warning = mention_audience_verdict("BigMama", &mixed, me.peer_id, "academy")
            .expect("unresolvable");
        assert!(!warning.contains(strong), "asserted absence: {warning}");
        assert!(warning.contains("1 of 2 peer(s)"), "vague: {warning}");

        // Empty roster — nobody seen at all cannot license it either.
        let empty =
            mention_audience_verdict("M5", &[], me.peer_id, "academy").expect("unresolvable");
        assert!(!empty.contains(strong), "asserted absence: {empty}");

        // All named and no match — the roster CAN answer, so the loud line is
        // earned. This is the #270 case the warning exists for.
        let named = vec![me.clone(), roster_member(Some("M5"))];
        let deaf =
            mention_audience_verdict("BigMama", &named, me.peer_id, "academy").expect("absent");
        assert!(deaf.contains(strong), "lost the real warning: {deaf}");
        // Both soft paths and the strong one carry the remediation that ended
        // #270 — it is true regardless of anyone's naming state.
        for line in [&warning, &empty, &deaf] {
            assert!(line.contains("--room general"), "lost remediation: {line}");
        }

        // A present, named match stays silent.
        assert!(mention_audience_verdict("m5", &named, me.peer_id, "academy").is_none());

        // Peer-id prefixes are two DIFFERENT properties and both need pinning:
        // an unnamed OTHER peer is still resolvable by id (so no warning), and
        // `heard` scanning the full roster means your OWN id resolves too —
        // otherwise self-exclusion would emit "@you has not been seen here".
        let other = roster_member(None);
        let other_prefix = other.peer_id.to_string()[..8].to_uppercase();
        let by_id = vec![me.clone(), other.clone()];
        assert!(mention_audience_verdict(&other_prefix, &by_id, me.peer_id, "academy").is_none());
        let own_prefix = me.peer_id.to_string()[..8].to_uppercase();
        assert!(mention_audience_verdict(&own_prefix, &by_id, me.peer_id, "academy").is_none());
    }

    /// what this catches (#1378 review, card 74e8e6af): two defects in the
    /// `all` guard itself, both of which put a number or a verdict in front of
    /// the operator that the roster does not support.
    ///
    /// 1. A roster INCLUDES self. An operator who never ran `airc identity set`
    ///    is an unnamed member of every room, so counting self would make
    ///    `unnamed > 0` permanently true — the #270 warning could never fire
    ///    again for exactly the fresh-install operator most likely to need it,
    ///    and the soft line would describe them to themselves as a third party.
    /// 2. `room_roster_in` yields one row per (peer, CLIENT SESSION), so two
    ///    agent tabs on one box are two rows sharing a peer id. Counting rows
    ///    and printing "peer(s)" is a fabricated number.
    ///
    /// Mutation check: counting `roster` instead of `others` fails the first
    /// assert; counting rows instead of distinct peer ids fails the second.
    #[test]
    fn the_count_is_distinct_peers_and_never_includes_the_sender() {
        let me = roster_member(None); // uncarded operator — the fresh-install shape

        // Self must not suppress the real warning, even unnamed.
        let named_other = vec![me.clone(), roster_member(Some("M5"))];
        let deaf = mention_audience_verdict("BigMama", &named_other, me.peer_id, "academy")
            .expect("absent");
        assert!(
            deaf.contains("will NEVER receive"),
            "self suppressed the #270 warning: {deaf}"
        );

        // Two sessions of ONE unnamed peer is one peer, not two — and the
        // named peer plus that one makes two, not three.
        let twice = roster_member(None);
        let mut second_session = twice.clone();
        second_session.runtime = "codex".to_string();
        let rows = vec![
            me.clone(),
            twice.clone(),
            second_session,
            roster_member(Some("M5")),
        ];
        let soft = mention_audience_verdict("BigMama", &rows, me.peer_id, "academy")
            .expect("unresolvable");
        assert!(
            soft.contains("1 of 2 peer(s)"),
            "counted rows, not peers: {soft}"
        );

        // A roster of only the sender cannot license anything.
        let alone = vec![me.clone()];
        let solo =
            mention_audience_verdict("M5", &alone, me.peer_id, "academy").expect("unresolvable");
        assert!(
            solo.contains("no peer other than yourself"),
            "counted self as an audience: {solo}"
        );
    }

    /// what this catches: the send receipt must NOT report the enrolled
    /// peer count as confirmed delivery. The old line was
    /// "sent to X — N paired peer(s) + any local scope tailing …",
    /// which made a send look successful even when zero peers actually
    /// received anything (the false "it works" bug). The receipt is
    /// honest: the verb is "queued"/"addressed" (not "sent to N peers"),
    /// it never claims "paired peer(s)" delivered, and it tells the
    /// operator delivery is asynchronous + how to confirm it.
    #[test]
    fn send_receipt_does_not_imply_confirmed_delivery() {
        // 41 enrolled, all currently connected → the healthy branch.
        let line = format_send_receipt("general", "cb2e21a1", 41, 41);

        // The exact misleading phrasing is gone.
        assert!(
            !line.contains("41 paired peer(s)"),
            "must not report enrolled count as delivery: {line}"
        );
        assert!(
            !line.contains("paired peer"),
            "the enrolled-peer count must not masquerade as delivery: {line}"
        );
        // It must not lead with the delivery-implying verb "sent to".
        assert!(
            !line.starts_with("sent to"),
            "verb must not imply confirmed delivery: {line}"
        );
        // It IS honest about what happened and what is unconfirmed.
        assert!(line.contains("queued to general"), "honest verb: {line}");
        assert!(
            line.contains("enrolled"),
            "the address-book count is still disclosed, just not as reach: {line}"
        );
        assert!(
            line.contains("asynchronous"),
            "must state delivery is unconfirmed: {line}"
        );
        assert!(
            line.contains("airc doctor --health"),
            "must tell operator how to confirm delivery: {line}"
        );
    }

    /// what this catches (#351): the receipt inviting the reader to compute a
    /// REACH RATIO out of two incommensurable numbers.
    ///
    /// It used to print "addressed 52 enrolled remote peer(s), 1 currently
    /// connected". Enrolled is every peer this scope ever met, across every room,
    /// for all time — not this room's audience — and the live count is LAN links
    /// only. Read as a fraction it says 2%. The ack ledger for the same period
    /// said 767 of 771 delivered: 99.5%. A card (#340) was filed on the strength
    /// of that line, and the line was the only thing wrong.
    ///
    /// An instrument that reads as catastrophe during healthy operation is worse
    /// than no instrument — it spends the trust that a REAL alarm will need.
    #[test]
    fn the_receipt_never_reads_as_a_reach_ratio() {
        // The shape that caused the false alarm: a big address book, one live link.
        let line = format_send_receipt("cambriantech", "cb2e21a1", 52, 1);

        // The adjacency itself was the bug — "N enrolled …, M currently connected"
        // sitting together is what the eye divides. Neither the old phrasing nor
        // the old ordering may come back.
        assert!(
            !line.contains("currently connected"),
            "a live-link count must not sit where it reads as the numerator: {line}"
        );
        assert!(
            !line.contains("addressed 52"),
            "the address book is not an audience and must not be 'addressed': {line}"
        );
        // It must say so outright, because a reader who has been burned once will
        // keep dividing unless told the denominator is not a denominator.
        assert!(
            line.contains("not this room's audience"),
            "must disclaim the address book explicitly: {line}"
        );
        // And it must point at the ACK ledger, which is what delivery actually
        // means (#280: delivery is a returned ack, never a connection's existence).
        assert!(
            line.contains("ACK") || line.contains("ack"),
            "delivery truth is the ack ledger, not this line: {line}"
        );
    }

    /// what this catches: the zero-enrolled-peer case stays honest —
    /// no false "delivered" claim, but it correctly preserves the
    /// load-bearing truth that same-machine tailers still receive the
    /// frame (the lone-node / two-scopes-one-home topology). Count is
    /// reported as enrolled, never as a delivery confirmation.
    #[test]
    fn send_receipt_zero_peers_is_honest_about_local_tailers() {
        let line = format_send_receipt("general", "cb2e21a1", 0, 0);
        assert!(!line.starts_with("sent to"), "no delivery verb: {line}");
        assert!(
            !line.contains("paired peer"),
            "no paired-peer claim: {line}"
        );
        assert!(line.contains("queued to general"), "honest verb: {line}");
        assert!(
            line.contains("0 enrolled remote peer(s)"),
            "honest about zero enrolled peers: {line}"
        );
        assert!(
            line.contains("tailing this channel on this machine"),
            "must preserve the same-machine-delivery truth: {line}"
        );
    }

    /// what this catches (issue #1243 — the legibility trap): enrolled
    /// peers exist but the daemon holds ZERO live LAN connections, so
    /// room broadcast forwarded to nobody. The receipt must say so
    /// LOUDLY (not report the enrolled "address book" count as if it
    /// were reach) — the silent-success of this exact case let a fully
    /// broken fan-out masquerade as a healthy channel for an hour.
    #[test]
    fn send_receipt_warns_loudly_when_no_peer_is_connected() {
        let line = format_send_receipt("general", "cb2e21a1", 42, 0);
        assert!(line.contains("queued to general"), "honest verb: {line}");
        // It must NOT frame 42 enrolled as if 42 received it.
        assert!(
            line.contains("reached 0 of 42"),
            "must surface that 0 of 42 enrolled peers were reached: {line}"
        );
        assert!(
            line.contains("NONE are currently connected"),
            "must name the broken-route cause, not imply delivery: {line}"
        );
        assert!(
            line.contains("airc doctor --health"),
            "must point the operator at the route diagnostic: {line}"
        );
        // The same-machine truth is preserved even when remotes are dark.
        assert!(
            line.contains("tailing this channel on this machine"),
            "local tailers still receive it: {line}"
        );
    }

    /// what this catches: the healthy branch reports BOTH counts —
    /// enrolled (address book) and the live-connected subset — without
    /// claiming confirmed delivery. Distinguishes "addressed N, M
    /// connected" from the loud zero-connected case above.
    #[test]
    fn send_receipt_reports_connected_subset_when_routes_are_up() {
        let line = format_send_receipt("general", "cb2e21a1", 42, 3);
        // Both facts are still disclosed — the fix was never about hiding a
        // number, it was about not pairing them as numerator and denominator.
        assert!(
            line.contains("3 peers"),
            "the live-link count is still reported: {line}"
        );
        assert!(
            line.contains("42 peer(s) are enrolled"),
            "the address-book size is still reported, as its own fact: {line}"
        );
        assert!(
            line.contains("asynchronous"),
            "still does not claim confirmed delivery: {line}"
        );
    }
}
