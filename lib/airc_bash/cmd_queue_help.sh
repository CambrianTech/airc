# Sourced by cmd_queue.sh. Queue command help text only.
# Keep behavior in cmd_queue.sh; keep operator-facing docs here.

_airc_queue_help() {
  cat <<'EOF'
airc queue — issue-backed work queue primitives (airc#562)

USAGE
  airc queue [<owner/repo>] [--limit N] [--json]
  airc queue plan [<owner/repo>] [--limit N] [--json]
  airc queue steward [<owner/repo>] [--stale-after 30m] [--json]
  airc queue add <owner/repo> --title "<one-line>" [card-fields...]
  airc queue list [<owner/repo>] [--owner X] [--status Y] [--limit N] [--json]
  airc queue claim <issue-url> [--owner X] [--status Y]
  airc queue release <issue-url> [--reason "..."] [--status claimed|blocked]
  airc queue set-status <issue-url> <state>
  airc queue heartbeat <issue-url> [--owner X] [--status Y] [--note "..."]
  airc queue stale <owner/repo> [--stale-after 30m]
  airc queue next [<owner/repo>] [--limit N] [--json]
  airc queue metronome <owner/repo> [--interval 300]
  airc queue nudge <issue-url> [--peer @handle] [--message "..."]
  airc queue nudge <owner/repo> [--message "..."] [--limit N]
  airc queue adopt <issue-url> [card-fields...] [--force]
  airc queue pongs <owner/repo> [--since 30m] [--sweep-id ID]
  airc queue availability <owner/repo> [--since 30m] [--stale-after 30m]
  airc queue close-merged <pr-url> [--merge-sha SHA] [--actor X] [--dry-run]
  airc queue staleness <pr-url|owner/repo#PR> [--repo-root PATH] [--json]

DESCRIPTION
  Plans, adds, lists, or mutates queue cards (GitHub issues with
  airc-queue label). Bare `airc queue` defaults to the cohesive planning
  view: priorities, lanes, active owners, stale claims, review blockers,
  and next actions. Card fields follow the spec in continuum/.airc/QUEUE.md
  (sibling claude tab #1's continuum#1110).

VERB SCOPE
  plan / priorities / kanban  Cohesive prioritized kanban for agents
  steward / digest / pm       Dry-run PM digestion and queue hygiene
  add / list                   PR-1 (airc#566, merged)
  claim / release / set-status PR-2 (airc#568, merged)
  heartbeat / stale            Stale-claim liveness primitives (airc#572)
  next / pick                  Idle-agent next-work recommendations
  metronome / pulse            Automatic queue-next idle pulse config
  nudge                        PR-3 (card) + PR-4a (repo ping/pong sweep)
  adopt / import               Backlog migration (airc#575)
  pongs / pong-summary          Repo-nudge response collection (airc#579)
  availability / avail          Live peer + stale-claim summary (airc#591)
  close-merged                 airc#576 — auto-close on PR merge to canary
  staleness / stale-pr          airc#615 — warn on PR branch/base drift
  auto-release policy          Deferred; stale is read-only first

EOF
}

_airc_queue_plan_help() {
  cat <<'EOF'
airc queue plan — cohesive prioritized kanban for agents

USAGE
  airc queue [<owner/repo>] [--limit N] [--json]
  airc queue plan [<owner/repo>] [--limit N] [--json]
  airc queue priorities [<owner/repo>]
  airc queue kanban [<owner/repo>]

DESCRIPTION
  One-command coordination view. It fetches open airc-queue cards and
  automatically groups them into strategic lanes, infers P0/P1/P2 priority,
  shows review/merge candidates, stale ownership, active owners, and concrete
  next actions. This is the default agent check-in command; use `--json` for
  tooling.

OPTIONS
  --repo <owner/repo>      Alternative way to specify repo.
  --limit <N>              Max cards to fetch (default 100).
  --stale-after <dur>      Heartbeat threshold for stale claims: 30m, 2h, 1d
                           (default: 30m).
  --owner <handle>         Owner used in generated claim commands.
  --json                   Emit machine-readable JSON.
  -h, --help               This help.

LANES
  alpha-gap/rust-runtime   Rust-owned cognition/runtime, ts-rs, provider/model logic.
  perf/resource-control    CPU/GPU/memory/docker/throughput/latency work.
  flywheel/automation      AIRC, queue, kanban, canary, PR/issue automation.
  quality/tests-vdd        Tests, lint, clippy, VDD/TDD, validation hygiene.
  ui/configurator          UI/browser/configurator-specific work.
  integration/canary       Merge/release/install/image integration.

EXAMPLES
  airc queue
  airc queue plan CambrianTech/continuum
  airc queue plan --json | jq '.summary'
EOF
}

_airc_queue_steward_help() {
  cat <<'EOF'
airc queue steward — dry-run PM digestion for queue hygiene

USAGE
  airc queue steward [<owner/repo>] [--stale-after 30m] [--limit N] [--owner HANDLE] [--json]
  airc queue digest [<owner/repo>]
  airc queue pm [<owner/repo>]

DESCRIPTION
  Read-only project-manager steward loop. It scans open airc-queue cards and
  proposes concrete maintenance actions: nudge stale claims, claim ready cards,
  fill missing next actions, review implicit P0s, detect overloaded owners, and
  flag priority collapse when almost everything has become P0.

  It does not mutate cards. The printed commands are recommendations that a
  human, agent, or future policy engine can choose to run.

OPTIONS
  --repo <owner/repo>      Alternative way to specify repo.
  --stale-after <dur>      Heartbeat threshold: 30m, 2h, 1d (default 30m).
  --limit <N>              Max issues to fetch (default 100).
  --owner <handle>         Owner used in suggested claim commands.
  --json                   Emit machine-readable JSON.
  -h, --help               This help.
EOF
}

_airc_queue_add_help() {
  cat <<'EOF'
airc queue add — create a new queue card

USAGE
  airc queue add <owner/repo> --title "<one-line>" [card-fields...] [--dry-run]

REQUIRED
  <owner/repo>           Target GitHub repo (e.g. CambrianTech/continuum)
  --title "<text>"       One-line card title

CARD FIELDS (all optional; defaults shown)
  --id <ref>             Issue/PR this card coordinates (e.g. #1085, airc#562)
  --branch <name>        Branch name (e.g. fix/install-tier-name)
  --owner <handle>       Queue owner (default: current work identity from
                         `airc identity whoami`)
  --status <state>       claimed | in-progress | blocked | review | merged
                         (default: claimed)
  --blockers <list>      Comma-separated #NNNN (e.g. "#1085, airc#559")
  --env <tag>            mac-m5 | rtx5090-wsl2 | linux-amd64-any | any
  --evidence <text>      Gates run + sha (e.g. "prepush 61bdeb407: 27/27")
  --next-action <text>   One sentence on next step
  --last-heartbeat <ts>  ISO timestamp + sha (e.g. "2026-05-13T17:35Z @ 61bdeb407")

OPTIONS
  --dry-run              Print the card body that WOULD be posted; don't post.
  -h, --help             This help.

ENVIRONMENT
  AIRC_QUEUE_OWNER        Compatibility fallback for older launchers.
  AIRC_AGENT_NAME/NICK    Compatibility fallback for older launchers.

EXAMPLES
  airc queue add CambrianTech/continuum \\
    --title "Implement Lane B-Mac MetalMonitor adapter" \\
    --owner "claude-tab-2" \\
    --branch "feat/lane-c-mac-metal-adapter" \\
    --env "mac-m5" \\
    --status "claimed" \\
    --next-action "Wait for RTX substrate schema then wire MetalMonitor into seam metadata"

NOTES
  - 'gh' CLI must be authenticated.
  - The 'airc-queue' label is auto-applied if it exists on the target
    repo; otherwise the issue posts without one and a hint suggests
    creating it.
EOF
}

_airc_queue_list_help() {
  cat <<'EOF'
airc queue list — list open queue cards

USAGE
  airc queue list [<owner/repo>] [--owner X] [--status Y] [--limit N] [--json]

ARGUMENTS
  <owner/repo>           Target GitHub repo. If omitted, auto-detected
                         from the current directory's git remote.

OPTIONS
  --repo <owner/repo>    Alternative way to specify repo (vs positional).
  --owner <handle>       Filter to cards owned by this handle.
  --status <state>       Filter to cards in this state.
  --limit <N>            Max cards to fetch (default 30; gh hard cap 100).
  --check-staleness      For review cards, run `airc queue staleness` on the
                         first linked PR ref and print warnings inline.
  --repo-root <path>     Git checkout used by --check-staleness.
  --no-fetch-staleness   Pass --no-fetch to the staleness sweep; useful for
                         local/offline validation with already-present refs.
  --json                 Emit JSON instead of human-readable text.
  -h, --help             This help.

EXAMPLES
  airc queue list CambrianTech/continuum
  airc queue list --status in-progress
  airc queue list --owner claude-tab-2 --json | jq '.[] | .url'

NOTES
  - Lists only OPEN airc-queue issues (closed = merged/done in PR-1).
  - Filters apply client-side after fetching matching issues by label.
EOF
}

_airc_queue_claim_help() {
  cat <<'EOF'
airc queue claim — take ownership of a queue card

USAGE
  airc queue claim <issue-url> [--owner X] [--status Y] [--force] [--dry-run]
  airc queue claim owner/repo#N [--owner X] [--status Y] [--force] [--dry-run]

DESCRIPTION
  Sets the card's owner field and status to indicate active work. Default
  owner = current work identity from `airc identity whoami`; default status = in-progress.
  Appends a "## Status log" line with timestamp + actor.

  Collision protection is automatic: claiming a card already owned by a
  different active owner fails before mutation. Use --force only for an
  intentional handoff or stale-owner takeover.

OPTIONS
  --owner <handle>   Queue owner to set (default: current work identity).
  --status <state>   New status (default: in-progress).
  --force            Override a different active owner.
  --dry-run          Print the new body that WOULD be written; don't edit.
  -h, --help         This help.
EOF
}

_airc_queue_release_help() {
  cat <<'EOF'
airc queue release — give up ownership of a queue card

USAGE
  airc queue release <issue-url> [--reason "..."] [--status claimed|blocked] [--dry-run]
  airc queue release owner/repo#N [--reason "..."] [--status claimed|blocked] [--dry-run]

DESCRIPTION
  Clears the owner field (back to the unclaimed pool) and sets status to
  "claimed" (default) or "blocked" if --status blocked. Appends a status
  log line with timestamp, actor, and optional reason.

OPTIONS
  --reason "<text>"  Brief explanation logged with the release.
  --status <state>   New status: claimed or blocked (default: claimed).
                     For in-progress/review/merged use `airc queue set-status`.
  --dry-run          Print what WOULD be written; don't edit.
  -h, --help         This help.
EOF
}

_airc_queue_set_status_help() {
  cat <<'EOF'
airc queue set-status — change the status field on a queue card

USAGE
  airc queue set-status <issue-url> <state> [--dry-run]
  airc queue set-status owner/repo#N <state> [--dry-run]

ARGUMENTS
  <state>            One of: claimed, in-progress, blocked, review, merged.

OPTIONS
  --dry-run          Print what WOULD be written; don't edit.
  -h, --help         This help.

NOTES
  - Does NOT close the issue automatically when set to merged. Operators
    close manually so the queue tracks closure events explicitly.
EOF
}

_airc_queue_heartbeat_help() {
  cat <<'EOF'
airc queue heartbeat — stamp liveness on a queue card

USAGE
  airc queue heartbeat <issue-url> [--owner X] [--status Y] [--note "..."] [--dry-run]
  airc queue heartbeat owner/repo#N [--owner X] [--status Y] [--note "..."] [--dry-run]

DESCRIPTION
  Sets owner (default: current work identity) and last_heartbeat to the
  current UTC timestamp plus git SHA when available. Optionally updates
  status. Appends a Status log line so humans can see that work is alive.

OPTIONS
  --owner <handle>   Queue owner to record (default: current work identity).
  --status <state>   Optional status update: claimed, in-progress, blocked,
                     review, or merged.
  --note "<text>"    Short context appended to the status log.
  --dry-run          Print what WOULD be written; don't edit.
  -h, --help         This help.
EOF
}

_airc_queue_stale_help() {
  cat <<'EOF'
airc queue stale — list owned queue cards with missing/old heartbeats

USAGE
  airc queue stale [<owner/repo>] [--stale-after 30m] [--limit N] [--json]

DESCRIPTION
  Read-only stale-claim scan. It lists open airc-queue cards in claimed,
  in-progress, or review state when they have an owner but no heartbeat,
  no owner, or a last_heartbeat older than --stale-after. It does not
  release or mutate cards; humans/agents can nudge, heartbeat, or release.

OPTIONS
  --repo <owner/repo>      Alternative way to specify repo.
  --stale-after <dur>      Duration threshold: 30m, 2h, 1d (default: 30m).
  --limit <N>              Max issues to fetch (default 50).
  --json                   Emit machine-readable JSON.
  -h, --help               This help.
EOF
}

_airc_queue_next_help() {
  cat <<'EOF'
airc queue next — recommend next claimable work for idle agents

USAGE
  airc queue next [<owner/repo>] [--owner HANDLE] [--limit N] [--json]
  airc queue pick [<owner/repo>] [--idle-ping]

DESCRIPTION
  Action-oriented flywheel primitive. Scans open airc-queue cards and ranks
  claimable work so an agent that just finished a task can immediately pick
  another one without waiting for a human. Every candidate includes exact
  `airc queue claim ...` and `airc lane create ...` commands.

OPTIONS
  --repo <owner/repo>      Alternative way to specify repo.
  --owner <handle>         Agent/work identity to use in claim commands.
                           Default: current AIRC work identity.
  --base <branch>          Base branch for suggested lane create commands
                           (default: canary).
  --repo-root <path>       Repo path for suggested lane create commands.
  --limit <N>              Max issues to fetch (default 30).
  --idle-ping              Broadcast that this agent is idle and looking
                           for work after printing recommendations.
  --json                   Emit machine-readable JSON.
  -h, --help               This help.

RANKING
  1. unowned claimed cards
  2. owned claimed cards
  3. unowned blocked cards
  4. review cards
  5. this owner's in-progress cards

NOTES
  - This does not mutate cards by itself. Agents should run the printed
    claim/lane commands, then heartbeat while working.
  - Monitor/automation layers should call this after a merge, release, or
    idle timeout; humans should not need to manually wake agents.
EOF
}

_airc_queue_metronome_help() {
  cat <<'EOF'
airc queue metronome — configure automatic queue-next idle pulses

USAGE
  airc queue metronome <owner/repo> [--interval 300] [--owner HANDLE] [--limit N]
  airc queue metronome <owner/repo> --all [--interval 300] [--roster-window 86400]
  airc queue metronome status
  airc queue metronome off

DESCRIPTION
  Stores a monitor-loop metronome config. While `airc join`/monitor is
  running, the monitor periodically runs a personalized dispatch:

    airc queue dispatch <handle> <owner/repo> --limit <N>

  That makes claimable work loud and addressed without waiting for a human
  to ask why agents are idle.

  With --all (airc#607 / continuum#1192), the monitor fans the dispatch
  out across every recent sender it has seen in this scope's message
  log — closes the gap where a single-owner config could only ever feed
  one agent while the rest of the room stayed idle.

OPTIONS
  --interval <seconds>   Pulse cadence. Minimum 30s; default 300s.
  --owner <handle>       Agent/work identity used for claim suggestions.
                         Default: current AIRC work identity. Mutually
                         exclusive with --all.
  --all, --roster        Fan-out mode: every pulse, iterate every recent
                         sender in this scope's messages.jsonl and DM
                         each one their next claimable card. Per-recipient
                         dedup window = pulse interval, so a tight cadence
                         can't double-ping the same agent.
  --roster-window <sec>  How far back to look for "recent sender" when
                         building the fan-out roster. Minimum 60s; default
                         86400 (24h). Only meaningful with --all.
  --limit <N>            Max queue cards to scan per pulse (default 10).
  --repo-root <path>     Optional repo path for suggested lane commands.
  status                 Show current config.
  off                    Disable metronome and clear last pulse timestamp.
  -h, --help             This help.

NOTES
  - The metronome does not claim work by itself. It DMs the target agent
    exact next commands so agents can claim deliberately.
  - This is intentionally separate from plain `airc reminder`: reminders
    only say "you are silent"; metronome says "here is what to do next."
  - Long-term, this dispatcher moves to a typed Rust substrate; see
    airc#628 (Rust queue-dispatch port).
EOF
}

_airc_queue_adopt_help() {
  cat <<'EOF'
airc queue adopt — convert an existing issue into a queue card

USAGE
  airc queue adopt <issue-url> [card-fields...] [--force] [--dry-run]
  airc queue adopt owner/repo#N [card-fields...] [--force] [--dry-run]

DESCRIPTION
  Prepends the standard airc-queue JSON envelope to an existing GitHub issue,
  preserves the original issue body under "Original issue body", and applies
  the airc-queue label when possible. This is the backlog migration path:
  existing issues become queue-managed cards without creating duplicates.

CARD FIELDS (all optional; defaults shown)
  --id <ref>             Issue/PR this card coordinates (default: #N)
  --branch <name>        Branch name, if known.
  --owner <handle>       Queue owner (default: current work identity)
  --status <state>       claimed | in-progress | blocked | review | merged
                         (default: claimed)
  --blockers <list>      Comma-separated blockers.
  --env <tag>            Environment/capability tag.
  --evidence <text>      Why this was adopted or what proof exists.
  --next-action <text>   One sentence on the next step.
  --last-heartbeat <ts>  ISO timestamp + sha, if known.

OPTIONS
  --force                Rewrite even if a queue envelope already exists.
  --dry-run              Print the adopted body that WOULD be posted.
  -h, --help             This help.

EXAMPLES
  airc queue adopt CambrianTech/continuum#914 \\
    --owner codex \\
    --status claimed \\
    --env ui/browser \\
    --next-action "Decide whether this stale UI issue still applies."
EOF
}

_airc_queue_nudge_help() {
  cat <<'EOF'
airc queue nudge — surface a queue card OR run a repo status sweep

USAGE
  airc queue nudge <issue-url> [--peer @handle] [--message "..."] [--dry-run]
  airc queue nudge owner/repo#N [--peer @handle] [--message "..."] [--dry-run]
  airc queue nudge owner/repo [--peer @handle] [--message "..."] [--limit N] [--sweep-id ID] [--dry-run]

ARGUMENTS
  <issue-url>        GitHub issue URL OR owner/repo#N reference. Card-scoped
                     nudge verifies kind=airc-queue-card-v1 and annotates it.
  owner/repo         Repo-scoped "Bueller" nudge. Broadcasts a status sweep
                     request to agents working in the current AIRC room/scope.

OPTIONS
  --peer @handle     DM the nudge to a specific peer. Default: broadcast to
                     the current scope's room.
  --message "..."    Optional one-line explanation appended to the nudge
                     ("nudge: #1125 — pickup needed before EOD" etc.).
  --limit N          Repo-scoped mode only: max queue cards to summarize
                     from the repo (default: 20).
  --sweep-id ID      Repo-scoped mode only: explicit sweep id. Default is
                     current UTC timestamp; replies should include sweep=ID.
  --dry-run          Print the broadcast text + status-log entry that
                     WOULD be written; don't send or edit.
  -h, --help         This help.

CARD-SCOPED MODE
  1. Verifies the issue is a real airc-queue card (envelope exists).
  2. Composes a one-line nudge: "nudge:<repo>#<N> [→ @peer] — <title> (<status>)
     [— <message>]"
  3. Sends via airc msg (broadcast OR DM if --peer), so peers see it in
     their inbox stream alongside other AIRC traffic.
  4. Appends a status-log entry to the card body recording who nudged + when
     + target peer (if any). Same _airc_queue_mutate_card path as
     claim/release/set-status — no new wire format.

REPO-SCOPED MODE
  - Lists open airc-queue cards on owner/repo, summarizes status/owner/branch.
  - Broadcasts a "repo-nudge:" ping asking online agents to pong with:
      identity, card/PR, state, blocker, next action, and keep/release claim.
  - Does NOT mutate cards yet. Future stale-claim automation consumes pongs.

NOTES
  - Nudge is the ACTION; stale-claim policy lives upstream/downstream.
  - Heartbeat / stall-detection (auto-pickup of cards whose owner went
    silent) is intentionally out of scope here — see airc#562 PR-4
    backlog and `.airc/ASSEMBLY-LINE.md` in continuum#1110.
  - Status fields are NOT changed by nudge. Use airc queue set-status if
    you need to mark a card differently.
EOF
}

_airc_queue_pongs_help() {
  cat <<'EOF'
airc queue pongs — summarize repo-nudge replies from the AIRC log

USAGE
  airc queue pongs <owner/repo> [--since 30m] [--sweep-id ID] [--limit N] [--json]
  airc queue pong-summary <owner/repo> [--since 30m] [--sweep-id ID]

DESCRIPTION
  Reads local AIRC messages.jsonl for `pong: owner/repo ...` replies,
  summarizes responders, and compares them to owners of open airc-queue
  cards. This is the audit half of `airc queue nudge owner/repo`.

OPTIONS
  --since <when>      ISO timestamp or relative window: 60s, 5m, 1h, 2d.
                      Default: 30m.
  --sweep-id <ID>     Only include pongs with sweep=<ID>.
  --limit <N>         Max queue cards and log lines to inspect (default 200).
  --json              Emit machine-readable summary.
  -h, --help          This help.

EXPECTED PONG
  pong: owner/repo — sweep=ID — <nick> — card=<owner/repo#N|idle>
    state=<idle|coding|testing|reviewing|blocked> blocker=<none|...>
    next=<...> claim=<keep|release|none>
EOF
}

_airc_queue_availability_help() {
  cat <<'EOF'
airc queue availability — summarize live queue ownership and peer activity

USAGE
  airc queue availability <owner/repo> [--since 30m] [--stale-after 30m] [--limit N] [--json]
  airc queue avail <owner/repo> [--since 30m]

DESCRIPTION
  Read-only flywheel / stale-claim view. Combines open airc-queue cards, last heartbeat
  fields, local AIRC messages, and repo-nudge pongs into one operator
  summary so agents can see who appears active, which claims need attention,
  and which nudge/pongs commands to run next.

OPTIONS
  --since <dur|ISO>       Recent-message window (default 30m).
  --stale-after <dur>     Claim heartbeat age considered stale (default 30m).
  --sweep-id <ID>         Suggested sweep id for the next repo nudge.
                          Defaults to current UTC timestamp.
  --limit <N>             Max queue cards / log lines to inspect (default 200).
  --json                  Emit machine-readable JSON.
  -h, --help              This help.

NOTES
  - Does not mutate cards or send messages.
  - For stale/missing owners, run the printed `airc queue nudge ...` command,
    then later run the printed `airc queue pongs ...` command.
EOF
}

_airc_queue_close_merged_help() {
  cat <<'EOF'
airc queue close-merged — auto-close queue cards completed by a merged PR

USAGE
  airc queue close-merged <pr-url> [--merge-sha SHA] [--actor X] [--allow-cross-repo] [--dry-run]
  airc queue close-merged owner/repo#PR [--merge-sha SHA] [--actor X] [--allow-cross-repo] [--dry-run]

ARGUMENTS
  <pr-url>            GitHub PR URL (https://github.com/.../pull/N) OR
                      owner/repo#N short form. PR must already be merged.

OPTIONS
  --merge-sha SHA     Merge commit SHA for the audit trail. If omitted,
                      pulled from PR metadata (mergeCommit.oid).
  --actor X           Identity recorded in the status-log entry. Defaults
                      to current work identity. CI passes
                      "github-actions" so the audit trail names the system.
  --allow-cross-repo  Attempt to close cross-repo queue-card refs (default:
                      report-only). Requires gh to be authenticated with a
                      token that has issues:write on the OTHER repo —
                      typically a fine-grained PAT or GitHub App
                      installation token, supplied via the workflow's
                      GH_TOKEN secret. Without this flag, cross-repo refs
                      are detected + reported but NOT closed (preserves
                      backward compat with existing repo-scoped workflows).
                      See continuum#1174 for the design rationale.
  --dry-run           Show what WOULD be closed; don't mutate or close.
  -h, --help          This help.

WHAT IT DOES
  1. Fetches the PR body via gh.
  2. Validates the PR is actually merged.
  3. Parses the body for same-repo and cross-repo queue-card closing refs
     with GitHub-style closing keywords (Closes/Fixes/Resolves).
  4. For same-repo queue cards: sets status=merged and closes the issue.
  5. Cross-repo closing refs are reported. With --allow-cross-repo set,
     attempts the close (gh's auth scope decides if it actually succeeds —
     failures count as errored, not silent skips). Without the flag, they
     are skipped with a count in the summary.
  Plain mentions like "Refs #N" are ignored so doc-only PRs do not close
  implementation cards.
EOF
}

_airc_queue_staleness_help() {
  cat <<'EOF'
airc queue staleness — warn when a PR branch would revert current-base work

USAGE
  airc queue staleness <pr-url|owner/repo#PR> [--repo-root PATH] [--json]
  airc queue stale-pr <pr-url|owner/repo#PR> [--repo-root PATH]
  airc queue staleness --base canary --head feat/x [--repo-root PATH] [--no-fetch]

DESCRIPTION
  Read-only PR freshness guard for queue review. It compares the PR head
  against the current base branch, limited to files touched by the PR, and
  reports base-side lines that are absent from the PR head. Those lines are
  the practical "this merge would erase already-merged work" signal that CI
  and GitHub's generic out-of-date warning do not explain.

OPTIONS
  --repo-root PATH    Git checkout to inspect. Default: current directory.
  --base REF          Base branch/ref when not using gh PR metadata.
  --head REF          Head branch/ref when not using gh PR metadata.
  --repo owner/repo   Repo for --pr mode.
  --pr N              PR number for --repo mode.
  --limit-lines N     Max missing base lines to print (default: 40).
  --no-fetch          Do not fetch; treat --base/--head as local git refs.
  --json              Emit machine-readable JSON.
  -h, --help          This help.

NOTES
  - With a PR URL or owner/repo#N, the command fetches origin/<base> and
    refs/pull/N/head before diffing.
  - Exit status is currently 0 for both OK and WARN so this can be used in
    queue/listing surfaces without breaking scripts. Callers should inspect
    the printed status or JSON warning_count.
EOF
}
