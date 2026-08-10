//! The transport: spawn an ACP agent, drive one turn, come back with something
//! the room can read.
//!
//! Everything security-relevant is decided in [`crate::policy`] — this module's
//! job is to be the honest plumbing between that decision and the wire. The two
//! places where plumbing can quietly become policy are marked below, because
//! both of them look like harmless defaults:
//!
//! 1. **Choosing which permission option to send back.** The SDK example picks
//!    `options.first()`, which is fine when a human is watching and wrong here —
//!    option order is the agent's choice, so "first" can mean "allow always".
//!    See [`choose_outcome`].
//! 2. **Answering `AllowAlways`.** That tells the agent to stop asking, which
//!    silently retires our policy from every future turn. See [`choose_outcome`].

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    InitializeRequest, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome, ToolKind,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Client, ConnectionTo};

use crate::policy::{PermissionDecision, ToolIdentity};
use crate::{admit_prompt, AgentSpec, AgentTurn, BridgeError};

/// Map an ACP `ToolKind` onto the string an operator writes in `toolsAllow`.
///
/// This is the ONE place the enum becomes config, so the vocabulary can't drift
/// between the gate and the documentation.
///
/// Derived from the `ToolKind` serde repr (`#[serde(rename_all = "snake_case")]`)
/// rather than a `match`: `ToolKind` is `#[non_exhaustive]`, so any match needs a
/// `_` arm — which the compiler FORCES and the production no-silent-fallback
/// clippy gate (`-D clippy::wildcard_enum_match_arm`) forbids. No match can win.
///
/// The serde repr sidesteps the match: a future fieldless SDK variant yields its
/// OWN snake_case name, stays absent from `toolsAllow`, and is still denied — a new
/// protocol capability arrives CLOSED, not open, and the denial names the real
/// kind instead of a blanket `"other"`. A future *data-carrying* variant can't
/// render to a plain string and falls to `"other"` (still ACP's own default deny
/// bucket) — fail-closed either way, with no wildcard.
fn kind_key(kind: &ToolKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "other".to_owned())
}

/// Read the tool's identity off the wire request.
///
/// Both fields are `Option` on the wire and stay `Option` here rather than being
/// defaulted into something confident. `ToolPolicy::decide` is what turns "the
/// agent didn't say" into a refusal; inventing a kind at this layer would hide
/// that fact from the decision that needs it.
fn identity_of(request: &RequestPermissionRequest) -> ToolIdentity {
    let fields = &request.tool_call.fields;
    ToolIdentity {
        kind: fields.kind.as_ref().map(kind_key),
        title: fields.title.clone(),
    }
}

/// Turn a decision into the outcome to send back, given the options the agent
/// offered.
///
/// Pure, and separated from the async handler so the selection rule is testable
/// without a subprocess — this is the function that decides whether a "yes"
/// means *this once* or *forever*.
///
/// Rules:
/// - Prefer the ONCE-scoped option in both directions. An `AllowAlways` reply
///   makes the agent cache the approval and stop sending permission requests,
///   which would leave [`crate::policy::ToolPolicy`] wired up, passing its tests, and never
///   consulted again. Answering `AllowOnce` keeps the policy in the loop on
///   every single turn, which is the whole point of having one.
/// - If the agent offers no once-scoped option, take the always-scoped one but
///   return a caveat, so the room is told the agent will remember. Silently
///   accepting a broader grant than we intended is the failure this avoids.
/// - If the agent offers no option of the needed polarity at all, cancel.
///   Cancelling is a refusal the protocol has a word for; guessing at an option
///   of the wrong polarity is not.
fn choose_outcome(
    decision: &PermissionDecision,
    options: &[PermissionOption],
) -> (RequestPermissionOutcome, Option<String>) {
    let (once, always) = if decision.is_allowed() {
        (
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::AllowAlways,
        )
    } else {
        (
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        )
    };

    let find = |want: &PermissionOptionKind| {
        options
            .iter()
            .find(|o| &o.kind == want)
            .map(|o| o.option_id.clone())
    };

    if let Some(id) = find(&once) {
        return (
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
            None,
        );
    }

    if let Some(id) = find(&always) {
        let caveat = if decision.is_allowed() {
            "the agent offered no once-only option, so it will remember this approval and may \
             stop asking"
        } else {
            "the agent offered no once-only option, so it will remember this refusal"
        };
        return (
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
            Some(caveat.to_string()),
        );
    }

    (RequestPermissionOutcome::Cancelled, None)
}

/// One ACP agent, reachable from a room.
///
/// ## Session lifetime (a known cost, not an oversight)
///
/// Each [`prompt`](Self::prompt) spawns the agent, runs one turn, and lets the
/// process exit. That is correct-but-costly: it pays full startup per message and
/// the agent remembers nothing between turns.
///
/// It is the honest first shape because the alternative — a long-lived session
/// per room — has an unmeasured context-growth cost and a liveness story
/// (what happens to a room whose agent died three hours ago) that deserves to be
/// built deliberately rather than fallen into. The design doc lists session
/// lifetime as an open question to settle by measurement; this is the version
/// that is obviously correct while that measurement is outstanding.
#[derive(Debug, Clone)]
pub struct AcpBridge {
    spec: AgentSpec,
}

impl AcpBridge {
    pub fn new(spec: AgentSpec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> &AgentSpec {
        &self.spec
    }

    /// Run one turn: `peer_id` said `text` in a room; get the agent's reply.
    ///
    /// Errors are typed by what an operator would DO about them — see
    /// [`BridgeError`]. In particular an agent that dies mid-turn surfaces as
    /// [`BridgeError::Exchange`] rather than an empty reply, so the room learns
    /// the difference between "it had nothing to say" and "it stopped existing".
    pub async fn prompt(&self, peer_id: &str, text: &str) -> Result<AgentTurn, BridgeError> {
        // Before anything is spawned: a peer who may not prompt must not be able
        // to make us fork a process.
        admit_prompt(&self.spec, peer_id)?;

        let agent =
            AcpAgent::from_str(&self.spec.command).map_err(|e| BridgeError::AgentUnavailable {
                command: self.spec.command.clone(),
                detail: e.to_string(),
            })?;

        run_turn(agent, &self.spec, text).await
    }
}

/// Drive one ACP turn over an already-constructed transport.
///
/// Split from [`AcpBridge::prompt`] so the protocol logic is substitutable:
/// `prompt` supplies a spawned subprocess, while tests supply an in-process fake
/// agent. Both drive THIS function, so what CI exercises is the same code the
/// room runs — a test against a re-implementation of the turn would prove
/// nothing about the turn.
pub(crate) async fn run_turn(
    transport: impl agent_client_protocol::ConnectTo<Client> + 'static,
    spec: &AgentSpec,
    text: &str,
) -> Result<AgentTurn, BridgeError> {
    {
        let workspace = spec
            .workspace
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        // Decisions are collected out of the permission callback, which runs on
        // the connection's task rather than ours. std::sync::Mutex is right here
        // precisely because nothing awaits while it is held.
        let decisions: Arc<Mutex<Vec<PermissionDecision>>> = Arc::new(Mutex::new(Vec::new()));

        let policy = spec.tools.clone();
        let sink = Arc::clone(&decisions);
        let prompt_text = text.to_string();

        // Did the agent ever speak ACP at all?
        //
        // This is how "could not start" is told apart from "the exchange
        // failed", and it is deliberately STRUCTURAL rather than a string match
        // on the SDK's error text. `AcpAgent::from_str` only PARSES the command
        // — a nonexistent program parses fine and fails later inside connect,
        // where the SDK reports a generic internal error carrying
        // "program not found". Matching that string would work until the SDK
        // reworded it.
        //
        // Whereas: if we never got an initialize response, the agent never spoke,
        // and that is precisely `AgentUnavailable` ("could not be spawned or did
        // not speak ACP") no matter which of those it was. The distinction the
        // operator needs — install something vs debug something — survives
        // rewordings this way.
        let spoke = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spoke_inner = Arc::clone(&spoke);

        let text_out = Client
            .builder()
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, _connection| {
                    let identity = identity_of(&request);
                    let mut decision = policy.decide(&identity);
                    let (outcome, caveat) = choose_outcome(&decision, &request.options);

                    // A grant we had to make broader than intended is recorded as
                    // such, so `refusal_lines()`/the room see the real terms.
                    if let (PermissionDecision::Allow { caveat: slot, .. }, Some(c)) =
                        (&mut decision, caveat)
                    {
                        *slot = Some(c);
                    }

                    if let Ok(mut guard) = sink.lock() {
                        guard.push(decision);
                    }

                    responder.respond(RequestPermissionResponse::new(outcome))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(transport, async move |cx: ConnectionTo<_>| {
                // `run_until` sends session/new but NOT initialize — the SDK's
                // working example initializes explicitly and so do we.
                cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                spoke_inner.store(true, std::sync::atomic::Ordering::SeqCst);

                cx.build_session(&workspace)
                    .block_task()
                    .run_until(async |mut session| {
                        session.send_prompt(&prompt_text)?;
                        // Buffers chunks to end-of-turn and returns once. A room
                        // wants one message, not a token stream.
                        session.read_to_string().await
                    })
                    .await
            })
            .await
            .map_err(|e| {
                if spoke.load(std::sync::atomic::Ordering::SeqCst) {
                    BridgeError::Exchange {
                        detail: e.to_string(),
                    }
                } else {
                    BridgeError::AgentUnavailable {
                        command: spec.command.clone(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let decisions = Arc::try_unwrap(decisions)
            .map(|m| m.into_inner().unwrap_or_default())
            .unwrap_or_else(|shared| shared.lock().map(|g| g.clone()).unwrap_or_default());

        Ok(AgentTurn {
            text: text_out,
            decisions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::PermissionOptionId;

    fn opt(id: &str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption::new(
            PermissionOptionId::from(id.to_string()),
            id.to_string(),
            kind,
        )
    }

    fn allow() -> PermissionDecision {
        PermissionDecision::Allow {
            label: "read".into(),
            caveat: None,
        }
    }

    fn deny() -> PermissionDecision {
        PermissionDecision::Deny {
            label: "execute".into(),
            reason: "not allow-listed".into(),
        }
    }

    fn selected_id(outcome: &RequestPermissionOutcome) -> Option<String> {
        match outcome {
            RequestPermissionOutcome::Selected(s) => Some(s.option_id.to_string()),
            _ => None,
        }
    }

    /// what this catches: the SDK example's `options.first()`. Option ORDER is
    /// the agent's choice, so an agent that lists "allow always" first would get
    /// a permanent grant out of a one-time decision. We select by KIND.
    #[test]
    fn selection_is_by_kind_not_by_position() {
        let options = vec![
            opt("always", PermissionOptionKind::AllowAlways),
            opt("once", PermissionOptionKind::AllowOnce),
        ];
        let (outcome, caveat) = choose_outcome(&allow(), &options);
        assert_eq!(selected_id(&outcome).as_deref(), Some("once"));
        assert!(caveat.is_none());
    }

    /// what this catches: silently retiring our own policy. Answering
    /// "always" makes the agent stop sending permission requests — ToolPolicy
    /// would keep passing its tests while never being consulted again.
    #[test]
    fn a_once_option_is_preferred_so_the_policy_stays_in_the_loop() {
        let options = vec![
            opt("once", PermissionOptionKind::AllowOnce),
            opt("always", PermissionOptionKind::AllowAlways),
        ];
        assert_eq!(
            selected_id(&choose_outcome(&allow(), &options).0).as_deref(),
            Some("once")
        );
    }

    /// what this catches: accepting a broader grant than we intended, quietly.
    /// If only "always" is on offer we still have to answer, but the room is
    /// told the terms changed.
    #[test]
    fn an_always_only_agent_still_gets_answered_but_the_room_is_told() {
        let options = vec![opt("always", PermissionOptionKind::AllowAlways)];
        let (outcome, caveat) = choose_outcome(&allow(), &options);
        assert_eq!(selected_id(&outcome).as_deref(), Some("always"));
        assert!(caveat.expect("caveat required").contains("remember"));
    }

    /// what this catches: a denial that picks an ALLOW option because it was the
    /// only one there. Wrong-polarity is not a near-miss, it is the opposite
    /// verdict; cancelling is the protocol's word for "no".
    #[test]
    fn a_denial_never_selects_an_allow_option() {
        let options = vec![
            opt("once", PermissionOptionKind::AllowOnce),
            opt("always", PermissionOptionKind::AllowAlways),
        ];
        let (outcome, _) = choose_outcome(&deny(), &options);
        assert!(
            matches!(outcome, RequestPermissionOutcome::Cancelled),
            "a deny with only allow-options must cancel, not select"
        );
    }

    #[test]
    fn a_denial_prefers_reject_once() {
        let options = vec![
            opt("rej-always", PermissionOptionKind::RejectAlways),
            opt("rej-once", PermissionOptionKind::RejectOnce),
        ];
        assert_eq!(
            selected_id(&choose_outcome(&deny(), &options).0).as_deref(),
            Some("rej-once")
        );
    }

    /// what this catches: an agent that offers no options at all leaving the
    /// turn hanging. Cancel is a real answer; silence is not.
    #[test]
    fn no_options_at_all_is_cancelled_rather_than_hung() {
        let (outcome, _) = choose_outcome(&allow(), &[]);
        assert!(matches!(outcome, RequestPermissionOutcome::Cancelled));
    }

    /// what this catches: the kind vocabulary drifting between the gate and the
    /// docs. These strings ARE the `toolsAllow` config surface.
    #[test]
    fn kind_keys_are_the_documented_config_vocabulary() {
        assert_eq!(kind_key(&ToolKind::Read), "read");
        assert_eq!(kind_key(&ToolKind::Execute), "execute");
        assert_eq!(kind_key(&ToolKind::SwitchMode), "switch_mode");
        // ACP's own default for an unclassified tool lands in the deny bucket.
        assert_eq!(kind_key(&ToolKind::Other), "other");
    }

    /// End-to-end turns against a real ACP agent — in-process, so the protocol
    /// is genuinely exercised (initialize → session/new → session/prompt →
    /// session/update → session/request_permission) with no subprocess and no
    /// network.
    ///
    /// These drive [`run_turn`], the SAME function the room path calls, rather
    /// than a re-implementation of it. The one thing they deliberately do NOT
    /// cover is process spawn and stdio framing — that is what the
    /// `acp-echo-agent` fixture binary and the live Windows run are for.
    mod round_trip {
        use super::*;
        use crate::{AgentSpec, PeerPolicy, ToolPolicy};
        use agent_client_protocol::schema::v1::{
            AgentCapabilities, ContentBlock, ContentChunk, InitializeResponse, NewSessionRequest,
            NewSessionResponse, PromptRequest, PromptResponse, SessionId, SessionNotification,
            SessionUpdate, StopReason, TextContent, ToolCallId, ToolCallUpdate,
            ToolCallUpdateFields,
        };
        use agent_client_protocol::{Agent, ConnectionTo, Responder};

        /// Build a fake agent that answers with `reply`, and — when
        /// `ask_for` is `Some(kind)` — first asks the client for permission to
        /// run a tool of that kind, appending the verdict it received to its
        /// reply so the test can see what the client actually sent back.
        fn fake_agent(
            reply: &'static str,
            ask_for: Option<ToolKind>,
            offer_once: bool,
        ) -> impl agent_client_protocol::ConnectTo<Client> + 'static {
            Agent
                .builder()
                .on_receive_request(
                    async move |req: InitializeRequest,
                                responder: Responder<InitializeResponse>,
                                _cx: ConnectionTo<Client>| {
                        responder.respond(
                            InitializeResponse::new(req.protocol_version)
                                .agent_capabilities(AgentCapabilities::new()),
                        )
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: NewSessionRequest,
                                responder: Responder<NewSessionResponse>,
                                _cx: ConnectionTo<Client>| {
                        responder.respond(NewSessionResponse::new(SessionId::new("fake-session")))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    move |req: PromptRequest,
                          responder: Responder<PromptResponse>,
                          cx: ConnectionTo<Client>| {
                        let ask_for = ask_for;
                        let cx2 = cx.clone();
                        // The turn runs in a SPAWNED task, not inline in the
                        // handler.
                        //
                        // An `on_receive_request` callback holds the connection's
                        // dispatch loop until it returns. Awaiting a nested
                        // `send_request(..).block_task()` inline therefore
                        // deadlocks by construction: the reply we are waiting for
                        // can only be delivered by the loop we are holding. The
                        // SDK documents this under "Deadlock Risk" and the first
                        // version of this fixture did it anyway — it hung for 20
                        // minutes with no output, which is why every test here now
                        // carries a deadline.
                        //
                        // (The BRIDGE side is not exposed to this: its permission
                        // handler decides purely and responds without awaiting any
                        // nested request, so it never holds the loop on a reply.)
                        let inline = async move {
                            let mut text = reply.to_string();

                            if let Some(kind) = ask_for {
                                let mut options = Vec::new();
                                if offer_once {
                                    options.push(opt("once", PermissionOptionKind::AllowOnce));
                                    options.push(opt("rej-once", PermissionOptionKind::RejectOnce));
                                } else {
                                    options.push(opt("always", PermissionOptionKind::AllowAlways));
                                }

                                let fields = ToolCallUpdateFields::new()
                                    .kind(kind)
                                    .title("touch a file".to_string());
                                let call = ToolCallUpdate::new(ToolCallId::new("call-1"), fields);

                                let verdict = cx
                                    .send_request(RequestPermissionRequest::new(
                                        req.session_id.clone(),
                                        call,
                                        options,
                                    ))
                                    .block_task()
                                    .await?;

                                text.push_str(&format!(" [verdict: {:?}]", verdict.outcome));
                            }

                            cx.send_notification(SessionNotification::new(
                                req.session_id,
                                SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                    ContentBlock::Text(TextContent::new(text)),
                                )),
                            ))?;
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        };
                        // Returns immediately, releasing the dispatch loop so the
                        // permission reply can actually be routed.
                        async move { cx2.spawn(inline) }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
        }

        /// Every round-trip below runs under a hard deadline.
        ///
        /// A deadlocked protocol test does not fail — it HANGS, and a hung CI job
        /// looks like a slow one until someone cancels it an hour later. (That is
        /// exactly what the first version of these tests did.) The deadline turns
        /// that failure mode into a normal red test with a message.
        async fn within<F, T>(what: &str, fut: F) -> T
        where
            F: std::future::Future<Output = T>,
        {
            match tokio::time::timeout(std::time::Duration::from_secs(20), fut).await {
                Ok(v) => v,
                Err(_) => panic!("{what} did not complete in 20s — the ACP exchange deadlocked"),
            }
        }

        /// what this catches: the whole point of the bridge. If initialize,
        /// session/new, prompt, or update assembly is wrong, a room gets silence
        /// — and silence is indistinguishable from an agent with nothing to say.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_prompt_comes_back_as_the_agents_reply() {
            let spec = AgentSpec::talk_only("unused-in-process");
            let turn = within(
                "plain reply",
                run_turn(fake_agent("hello from the agent", None, true), &spec, "hi"),
            )
            .await
            .expect("turn should complete");

            assert_eq!(turn.text, "hello from the agent");
            assert!(!turn.had_refusal(), "no tools were requested");
            assert!(turn.refusal_lines().is_empty());
        }

        /// what this catches: the deny-by-default gate being wired to nothing.
        /// An unconfigured agent must actually REFUSE on the wire — and the
        /// refusal must reach the room, not just a log.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_unconfigured_agent_refuses_a_tool_and_says_so() {
            let spec = AgentSpec::talk_only("unused-in-process");
            let turn = within(
                "refusal",
                run_turn(
                    fake_agent("tried to edit", Some(ToolKind::Edit), true),
                    &spec,
                    "edit something",
                ),
            )
            .await
            .expect("turn should complete");

            assert!(
                turn.had_refusal(),
                "an unconfigured lane must refuse: {turn:?}"
            );
            let lines = turn.refusal_lines();
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(lines[0].contains("toolsAllow"), "{lines:?}");
            // And the agent must have actually been told "no" on the wire, not
            // merely had a refusal recorded on our side.
            assert!(turn.text.contains("rej-once"), "agent saw: {}", turn.text);
        }

        /// what this catches: an allow-list that is decorative. A permitted kind
        /// must reach the agent as a real approval, and must NOT produce a room
        /// line (approvals stay quiet).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_allow_listed_kind_is_approved_on_the_wire() {
            let spec =
                AgentSpec::talk_only("unused-in-process").with_tools(ToolPolicy::allow(["edit"]));
            let turn = within(
                "approval",
                run_turn(
                    fake_agent("edited", Some(ToolKind::Edit), true),
                    &spec,
                    "edit something",
                ),
            )
            .await
            .expect("turn should complete");

            assert!(!turn.had_refusal(), "{turn:?}");
            assert!(turn.refusal_lines().is_empty(), "approvals stay quiet");
            assert!(turn.text.contains("once"), "agent saw: {}", turn.text);
        }

        /// what this catches: silently accepting a broader grant than we chose.
        /// When the agent offers only "always", the approval still happens but
        /// the room is told the agent will remember.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_always_only_grant_is_reported_to_the_room() {
            let spec =
                AgentSpec::talk_only("unused-in-process").with_tools(ToolPolicy::allow(["edit"]));
            let turn = within(
                "always-only grant",
                run_turn(
                    fake_agent("edited", Some(ToolKind::Edit), false),
                    &spec,
                    "edit something",
                ),
            )
            .await
            .expect("turn should complete");

            let caveated = turn
                .decisions
                .iter()
                .any(|d| d.to_room_line().contains("remember"));
            assert!(caveated, "terms change must be visible: {turn:?}");
        }

        /// what this catches: a refused peer still costing us an agent run. The
        /// gate must fire before the transport is ever built.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_refused_peer_never_reaches_the_agent() {
            let spec = AgentSpec::talk_only("definitely-not-a-real-command-xyz")
                .with_peers(PeerPolicy::only(["peer-a"]));
            let bridge = AcpBridge::new(spec);
            let err = bridge.prompt("peer-b", "hi").await.unwrap_err();
            assert!(
                matches!(err, BridgeError::PeerRefused { .. }),
                "expected a peer refusal, got {err:?}"
            );
        }
    }
}
