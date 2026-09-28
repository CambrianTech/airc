//! GitHub issue mirror, slice 1: the ISSUE a work card should have, and what to do about
//! the one GitHub holds (card 9681e5b5, Codex's adapter design).
//!
//! Joel, 2026-09-28: the cards of a project should be visible, and who did what, just by
//! going to GitHub. The board stays the source of truth for live work; the issue is its
//! readable, durable projection, written behind it and never on the claim path.
//!
//! Everything here is PURE: no network, no clock, no identity. The runtime (the mirror
//! process) resolves names, signs [`metadata_payload`], reads the remote issue, and
//! applies the returned [`MirrorAction`]. So every rule below is pinned by a test:
//! - the card owns only `airc:`-namespaced labels and the `airc-card:<id>` lookup label;
//!   a human's own labels are never touched;
//! - a field someone else edited on GitHub (it matches neither what the card wants nor
//!   what the mirror last pushed) is a visible CONFLICT, never silently overwritten.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::model::{CardState, Priority, WorkCard};

/// The label prefix the mirror owns. Everything else on an issue belongs to people.
pub const LABEL_NAMESPACE: &str = "airc:";

/// Opens the hidden metadata block in an issue body.
pub const METADATA_OPEN: &str = "<!-- airc-card:v1 ";

/// The label that finds a card's issue again after an uncertain write (a create that
/// timed out, a lost mirror state): `airc-card:<uuid>`, 46 characters, under GitHub's 50.
pub fn card_label(card: &WorkCard) -> String {
    format!("airc-card:{}", card.card_id.as_uuid())
}

/// Whether a label is one the mirror manages (and may add or remove).
pub fn is_managed_label(label: &str) -> bool {
    label.starts_with(LABEL_NAMESPACE) || label.starts_with("airc-card:")
}

/// An issue's open or closed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Open,
    Closed,
}

impl From<CardState> for IssueState {
    fn from(state: CardState) -> Self {
        match state {
            CardState::Open
            | CardState::Claimed
            | CardState::InProgress
            | CardState::Blocked
            | CardState::Review => IssueState::Open,
            CardState::Merged | CardState::Closed => IssueState::Closed,
        }
    }
}

fn lifecycle_label(state: CardState) -> &'static str {
    match state {
        CardState::Open => "airc:state/open",
        CardState::Claimed => "airc:state/claimed",
        CardState::InProgress => "airc:state/in-progress",
        CardState::Blocked => "airc:state/blocked",
        CardState::Review => "airc:state/review",
        CardState::Merged => "airc:state/merged",
        CardState::Closed => "airc:state/closed",
    }
}

fn priority_label(priority: Priority) -> &'static str {
    match priority {
        Priority::P0 => "airc:priority/p0",
        Priority::P1 => "airc:priority/p1",
        Priority::P2 => "airc:priority/p2",
        Priority::P3 => "airc:priority/p3",
    }
}

/// A card is a work card, or a review of another card.
fn kind_label(card: &WorkCard) -> &'static str {
    match card.reviews {
        Some(_) => "airc:kind/review",
        None => "airc:kind/work",
    }
}

/// A participant as the issue shows them: their airc name when they have one, and their
/// short peer id always, so the person behind a shared GitHub account is never guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub name: Option<String>,
    pub peer_short: String,
}

impl std::fmt::Display for Participant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{name} (airc peer {})", self.peer_short),
            None => write!(f, "unnamed (airc peer {})", self.peer_short),
        }
    }
}

/// Everything the issue renders, resolved by the runtime: the card, its review cards, and
/// who created and holds it.
#[derive(Debug, Clone)]
pub struct CardView<'a> {
    pub card: &'a WorkCard,
    pub reviews: Vec<&'a WorkCard>,
    pub created_by: Participant,
    pub holder: Option<Participant>,
}

/// The issue the card wants. `labels` holds only the labels the mirror manages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueSpec {
    pub title: String,
    pub body: String,
    pub state: IssueState,
    pub labels: BTreeSet<String>,
}

/// The canonical payload the runtime signs with its airc identity: which card, on which
/// repo, at which board revision. Field order is fixed, so the same card revision always
/// yields the same bytes.
pub fn metadata_payload(card: &WorkCard) -> String {
    format!(
        "{{\"v\":1,\"card\":\"{}\",\"repo\":\"{}\",\"updated_at_ms\":{}}}",
        card.card_id.as_uuid(),
        card.repo.as_str(),
        card.updated_at_ms
    )
}

fn short(uuid: &uuid::Uuid) -> String {
    uuid.to_string().chars().take(8).collect()
}

fn state_word(state: CardState) -> &'static str {
    lifecycle_label(state).trim_start_matches("airc:state/")
}

/// The issue body: the card's own text, then a rendered section saying where the card
/// stands and who did what, then the hidden signed metadata block.
pub fn render_body(view: &CardView<'_>, signature: Option<&str>) -> String {
    let card = view.card;
    let mut body = card.body.clone().unwrap_or_default();
    if !body.is_empty() {
        body.push_str("\n\n");
    }
    body.push_str("---\n");
    body.push_str(&format!(
        "**airc card** `{}`\n\n",
        short(&card.card_id.as_uuid())
    ));
    body.push_str(&format!("- State: {}\n", state_word(card.state)));
    body.push_str(&format!(
        "- Priority: {}\n",
        priority_label(card.priority).trim_start_matches("airc:priority/")
    ));
    body.push_str(&format!("- Created by: {}\n", view.created_by));
    match &view.holder {
        Some(holder) => body.push_str(&format!("- Held by: {holder}\n")),
        None => body.push_str("- Held by: nobody\n"),
    }
    if let Some(parent) = card.reviews {
        body.push_str(&format!("- Reviews card `{}`\n", short(&parent.as_uuid())));
    }
    for review in &view.reviews {
        body.push_str(&format!(
            "- Review `{}`: {}\n",
            short(&review.card_id.as_uuid()),
            state_word(review.state)
        ));
    }
    if let Some(pr) = &card.pull_request {
        body.push_str(&format!(
            "- Pull request: https://github.com/{}/pull/{}\n",
            pr.repo.as_str(),
            pr.number
        ));
    }
    if !card.submissions.is_empty() {
        body.push_str(&format!("- Submissions: {}\n", card.submissions.len()));
    }
    body.push_str(&format!("\n{METADATA_OPEN}{}", metadata_payload(card)));
    if let Some(signature) = signature {
        body.push_str(&format!(" sig={signature}"));
    }
    body.push_str(" -->\n");
    body
}

/// The issue a card wants.
pub fn desired_issue(view: &CardView<'_>, signature: Option<&str>) -> IssueSpec {
    let card = view.card;
    let labels = [
        lifecycle_label(card.state).to_string(),
        priority_label(card.priority).to_string(),
        kind_label(card).to_string(),
        card_label(card),
    ]
    .into_iter()
    .collect();
    IssueSpec {
        title: card.title.clone(),
        body: render_body(view, signature),
        state: IssueState::from(card.state),
        labels,
    }
}

/// The issue as GitHub holds it now (all labels, the mirror's and people's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: IssueState,
    pub labels: BTreeSet<String>,
}

/// A field of an issue the mirror writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueField {
    Title,
    Body,
    State,
}

/// What the mirror does next for one card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorAction {
    /// No issue yet: create it (after looking it up by [`card_label`], so an uncertain
    /// earlier create is found rather than duplicated).
    Create(IssueSpec),
    /// Bring the issue to `spec`: only the named fields change, and labels are set to
    /// `labels` (the managed ones replaced, people's kept).
    Update {
        number: u64,
        fields: BTreeSet<IssueField>,
        spec: IssueSpec,
        labels: BTreeSet<String>,
    },
    /// Already what the card wants.
    Unchanged,
    /// Someone else changed these fields on GitHub since the mirror last wrote them. They
    /// are left as they are and shown as a conflict, to be resolved by a typed edit.
    Conflict {
        number: u64,
        fields: BTreeSet<IssueField>,
    },
}

fn remote_value(remote: &RemoteIssue, which: IssueField) -> FieldValue<'_> {
    match which {
        IssueField::Title => FieldValue::Text(&remote.title),
        IssueField::Body => FieldValue::Text(&remote.body),
        IssueField::State => FieldValue::State(remote.state),
    }
}

fn spec_value(spec: &IssueSpec, which: IssueField) -> FieldValue<'_> {
    match which {
        IssueField::Title => FieldValue::Text(&spec.title),
        IssueField::Body => FieldValue::Text(&spec.body),
        IssueField::State => FieldValue::State(spec.state),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldValue<'a> {
    Text(&'a str),
    State(IssueState),
}

/// PURE: the next action for a card, from the issue GitHub holds (`None` when none was
/// found, by mapping or by [`card_label`]), what the mirror last pushed for it, and what
/// the card wants now.
///
/// A field the remote shows differently from what the card wants is ours to change only
/// when the remote still shows what the mirror last pushed. With no record of a last push
/// (a lost mirror state), an issue whose body carries the mirror's metadata block is the
/// mirror's own and is adopted; any other differing field is a conflict.
pub fn next_action(
    remote: Option<&RemoteIssue>,
    last_pushed: Option<&IssueSpec>,
    desired: &IssueSpec,
) -> MirrorAction {
    let Some(remote) = remote else {
        return MirrorAction::Create(desired.clone());
    };
    let ours_without_record = last_pushed.is_none() && remote.body.contains(METADATA_OPEN);
    let mut update = BTreeSet::new();
    let mut conflict = BTreeSet::new();
    for which in [IssueField::Title, IssueField::Body, IssueField::State] {
        let (now, want) = (remote_value(remote, which), spec_value(desired, which));
        if now == want {
            continue;
        }
        let owned = match last_pushed {
            Some(pushed) => spec_value(pushed, which) == now,
            None => ours_without_record,
        };
        if owned {
            update.insert(which);
        } else {
            conflict.insert(which);
        }
    }
    if !conflict.is_empty() {
        return MirrorAction::Conflict {
            number: remote.number,
            fields: conflict,
        };
    }
    let mut labels: BTreeSet<String> = remote
        .labels
        .iter()
        .filter(|l| !is_managed_label(l))
        .cloned()
        .collect();
    labels.extend(desired.labels.iter().cloned());
    if update.is_empty() && labels == remote.labels {
        return MirrorAction::Unchanged;
    }
    MirrorAction::Update {
        number: remote.number,
        fields: update,
        spec: desired.clone(),
        labels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{RepoId, WorkCardId};
    use airc_core::PeerId;

    fn card(state: CardState) -> WorkCard {
        WorkCard {
            card_id: WorkCardId::new(),
            repo: RepoId::new("CambrianTech/career-wrangler").expect("repo"),
            title: "App skeleton and ORM migrations".into(),
            body: Some("Slice 1 of the data model.".into()),
            priority: Priority::P1,
            lane_id: None,
            state,
            owner: None,
            claim_id: None,
            claim_provenance: None,
            claim_expires_at_ms: None,
            last_heartbeat_at_ms: None,
            pull_request: None,
            created_by: PeerId::new(),
            created_at_ms: 1,
            updated_at_ms: 2,
            reviews: None,
            submissions: Vec::new(),
            last_submission_rejection: None,
        }
    }

    fn view(card: &WorkCard) -> CardView<'_> {
        CardView {
            card,
            reviews: Vec::new(),
            created_by: Participant {
                name: Some("Kimi".into()),
                peer_short: "e2f0e022".into(),
            },
            holder: Some(Participant {
                name: Some("Kimi".into()),
                peer_short: "e2f0e022".into(),
            }),
        }
    }

    fn remote_of(spec: &IssueSpec) -> RemoteIssue {
        RemoteIssue {
            number: 7,
            title: spec.title.clone(),
            body: spec.body.clone(),
            state: spec.state,
            labels: spec.labels.clone(),
        }
    }

    // what this catches: the issue losing who did what (Joel: "see that it was Kimi that
    // made the edit, not the GitHub user joelteply"), or the card's state, priority and
    // kind not readable as labels.
    #[test]
    fn the_issue_names_who_did_what_and_labels_where_the_card_stands() {
        let c = card(CardState::Claimed);
        let spec = desired_issue(&view(&c), Some("SIG"));
        assert!(spec
            .body
            .contains("- Created by: Kimi (airc peer e2f0e022)"));
        assert!(spec.body.contains("- Held by: Kimi (airc peer e2f0e022)"));
        assert!(spec.body.contains(&format!(
            "{METADATA_OPEN}{} sig=SIG -->",
            metadata_payload(&c)
        )));
        assert_eq!(spec.state, IssueState::Open);
        for label in [
            "airc:state/claimed",
            "airc:priority/p1",
            "airc:kind/work",
            card_label(&c).as_str(),
        ] {
            assert!(spec.labels.contains(label), "{label}");
        }
        let unnamed = Participant {
            name: None,
            peer_short: "9bb24964".into(),
        };
        assert_eq!(
            unnamed.to_string(),
            "unnamed (airc peer 9bb24964)",
            "never a bare id, never a guessed name"
        );
        assert_eq!(IssueState::from(CardState::Merged), IssueState::Closed);
        assert_eq!(IssueState::from(CardState::Review), IssueState::Open);
    }

    // what this catches: the mirror overwriting a person's edit on GitHub, or touching
    // their labels. A field that matches neither what the card wants nor what the mirror
    // last pushed is a conflict; the mirror's own stale fields update; people's labels
    // stay. The positive controls: no issue creates, and an up-to-date issue is left alone.
    #[test]
    fn a_human_edit_is_a_conflict_and_their_labels_survive() {
        let before = card(CardState::Claimed);
        let pushed = desired_issue(&view(&before), None);
        let mut after = before.clone();
        after.state = CardState::Review;
        after.updated_at_ms = 3;
        let want = desired_issue(&view(&after), None);

        assert!(matches!(
            next_action(None, None, &want),
            MirrorAction::Create(_)
        ));
        assert_eq!(
            next_action(Some(&remote_of(&want)), Some(&want), &want),
            MirrorAction::Unchanged
        );

        let mut remote = remote_of(&pushed);
        remote.labels.insert("good first issue".into());
        let MirrorAction::Update { fields, labels, .. } =
            next_action(Some(&remote), Some(&pushed), &want)
        else {
            panic!("the mirror's own stale fields update");
        };
        assert!(fields.contains(&IssueField::Body));
        assert!(
            labels.contains("good first issue"),
            "a person's label is kept"
        );
        assert!(labels.contains("airc:state/review") && !labels.contains("airc:state/claimed"));

        let mut edited = remote_of(&pushed);
        edited.title = "Joel renamed this on GitHub".into();
        assert_eq!(
            next_action(Some(&edited), Some(&pushed), &want),
            MirrorAction::Conflict {
                number: 7,
                fields: [IssueField::Title].into_iter().collect()
            }
        );

        // lost mirror state: an issue carrying the mirror's metadata block is adopted, a
        // stranger's issue with differing fields is not
        assert!(matches!(
            next_action(Some(&remote_of(&pushed)), None, &want),
            MirrorAction::Update { .. }
        ));
        let mut stranger = remote_of(&pushed);
        stranger.body = "someone else's issue".into();
        assert!(matches!(
            next_action(Some(&stranger), None, &want),
            MirrorAction::Conflict { .. }
        ));
    }
}
