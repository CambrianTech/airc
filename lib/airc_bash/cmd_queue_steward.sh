# shellcheck shell=bash
# Sourced by cmd_queue.sh. Read-only queue steward / PM digestion view.

_cmd_queue_steward() {
  local target_repo=""
  local limit=100
  local output_json=0
  local stale_after="30m"
  local owner=""

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_steward_help
        return 0
        ;;
      --repo)        shift; target_repo="${1:-}" ;;
      --limit)       shift; limit="${1:-100}" ;;
      --stale-after) shift; stale_after="${1:-30m}" ;;
      --owner)       shift; owner="${1:-}" ;;
      --json)        output_json=1 ;;
      -*) die "queue steward: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue steward: too many positional args (use: queue steward [owner/repo])"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    die "queue steward: no <owner/repo> given and could not detect one from \$PWD's git remote. Run inside a GitHub checkout or pass --repo owner/repo."
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue steward: target must be owner/repo, got: $target_repo" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue steward: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue steward: --limit must be >= 1 (got: $limit)"
  fi
  if [ -z "$owner" ]; then
    owner=$(_airc_queue_resolve_name)
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue steward: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,updatedAt,createdAt 2>&1); then
    die "queue steward: gh issue list failed for $target_repo: $raw_json"
  fi

  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-steward.XXXXXX") || die "queue steward: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  AIRC_QUEUE_STEWARD_OWNER="$owner" \
  "$AIRC_PYTHON" - "$target_repo" "$stale_after" "$output_json" "$raw_json_file" <<'PYEOF'
import datetime as dt
import json
import os
import re
import sys
from collections import Counter, defaultdict

repo, stale_after_raw, output_json_raw, path = sys.argv[1:5]
output_json = output_json_raw == "1"
owner = os.environ.get("AIRC_QUEUE_STEWARD_OWNER") or "anonymous"

CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
ACTIVE_STATUSES = {"claimed", "in-progress", "review", "blocked"}
P0_WORDS = (
    "broken", "failing", "fail", "regression", "panic", "crash", "blocked",
    "idle", "flywheel", "canary", "main", "merge", "stale", "wrong",
)
P1_WORDS = (
    "rust", "perf", "speed", "memory", "cpu", "gpu", "resource", "ts-rs",
    "test", "vdd", "tdd", "cleanup", "refactor", "docker",
)

def parse_duration(value: str) -> dt.timedelta:
    match = re.fullmatch(r"\s*(\d+)\s*([smhd])\s*", value or "")
    if not match:
        print(f"queue steward: cannot parse --stale-after '{value}' (use 30m, 2h, 1d)", file=sys.stderr)
        sys.exit(2)
    amount = int(match.group(1))
    unit = match.group(2)
    if unit == "s":
        return dt.timedelta(seconds=amount)
    if unit == "m":
        return dt.timedelta(minutes=amount)
    if unit == "h":
        return dt.timedelta(hours=amount)
    return dt.timedelta(days=amount)

def parse_card(body: str) -> dict:
    for match in CARD_BLOCK_RE.finditer(body or ""):
        try:
            parsed = json.loads(match.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            return parsed
    return {}

def parse_time(value: str):
    if not value:
        return None
    match = re.search(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?Z)", value)
    if not match:
        return None
    raw = match.group(1)
    fmt = "%Y-%m-%dT%H:%M:%SZ" if raw.count(":") == 2 else "%Y-%m-%dT%H:%MZ"
    return dt.datetime.strptime(raw, fmt).replace(tzinfo=dt.timezone.utc)

def compact_title(title: str) -> str:
    return (title or "").replace("airc-queue: ", "", 1).strip()

def text_for(issue: dict, card: dict) -> str:
    return "\n".join([
        compact_title(issue.get("title", "")),
        issue.get("body") or "",
        str(card.get("next_action") or ""),
        str(card.get("evidence") or ""),
        str(card.get("env") or ""),
        str(card.get("branch") or ""),
    ]).lower()

def explicit_priority(card: dict) -> str:
    value = str(card.get("priority") or card.get("prio") or "").upper().strip()
    if value in {"P0", "P1", "P2", "P3"}:
        return value
    return ""

def priority_for(status: str, text: str, card: dict, stale: bool) -> str:
    explicit = explicit_priority(card)
    if explicit:
        return explicit
    if status in {"blocked", "review"} or stale or any(word in text for word in P0_WORDS):
        return "P0"
    if any(word in text for word in P1_WORDS):
        return "P1"
    return "P2"

def short(value: str, limit: int = 120) -> str:
    value = " ".join((value or "").split())
    if len(value) <= limit:
        return value
    return value[: limit - 1].rstrip() + "..."

def action(kind: str, card: dict | None, reason: str, command: str, severity: str = "info") -> dict:
    return {
        "kind": kind,
        "severity": severity,
        "ref": card["ref"] if card else "",
        "title": card["title"] if card else "",
        "reason": reason,
        "command": command,
    }

with open(path, "r", encoding="utf-8") as f:
    issues = json.load(f)

now = dt.datetime.now(dt.timezone.utc)
stale_after = parse_duration(stale_after_raw)
cards = []
for issue in issues:
    card = parse_card(issue.get("body", "") or "")
    if not card:
        continue
    status = (card.get("status") or "claimed").strip() or "claimed"
    card_owner = (card.get("owner") or "").strip()
    if card_owner == "unclaimed":
        card_owner = ""
    heartbeat = (card.get("last_heartbeat") or "").strip()
    heartbeat_at = parse_time(heartbeat)
    stale_reason = ""
    if status in {"claimed", "in-progress", "review"}:
        if card_owner and not heartbeat_at:
            stale_reason = "missing-heartbeat"
        elif card_owner and heartbeat_at and now - heartbeat_at > stale_after:
            stale_reason = "stale-heartbeat"
        elif not card_owner and status in {"in-progress", "review"}:
            stale_reason = "missing-owner"
    text = text_for(issue, card)
    ref = f"{repo}#{issue.get('number')}"
    priority = priority_for(status, text, card, bool(stale_reason))
    cards.append({
        "number": issue.get("number"),
        "ref": ref,
        "title": compact_title(issue.get("title", "")),
        "status": status,
        "owner": card_owner,
        "priority": priority,
        "explicit_priority": explicit_priority(card),
        "next_action": (card.get("next_action") or "").strip(),
        "evidence": (card.get("evidence") or "").strip(),
        "last_heartbeat": heartbeat,
        "stale_reason": stale_reason,
    })

actions = []
for card in cards:
    if card["status"] == "merged":
        actions.append(action(
            "close-merged-card",
            card,
            "queue card is marked merged but the GitHub issue is still open",
            f"gh issue close {card['ref']}",
            "info",
        ))
        continue
    if card["stale_reason"]:
        actions.append(action(
            "nudge-stale-claim",
            card,
            card["stale_reason"],
            f"airc queue nudge {card['ref']} --reason stale-claim --message \"Please heartbeat, release, or update next_action.\"",
            "warn",
        ))
    if card["status"] in {"claimed", "blocked"} and not card["owner"]:
        actions.append(action(
            "claim-ready-card",
            card,
            "claimable card has no active owner",
            f"airc queue claim '{card['ref']}' --owner '{owner}'",
            "info",
        ))
    if not card["next_action"]:
        actions.append(action(
            "fill-next-action",
            card,
            "missing next_action makes the card hard to dispatch",
            f"airc queue heartbeat {card['ref']} --note \"Set next_action before dispatch.\"",
            "warn",
        ))
    if card["priority"] == "P0" and not card["explicit_priority"] and not card["stale_reason"] and card["status"] not in {"blocked", "review"}:
        actions.append(action(
            "review-priority",
            card,
            "implicit P0 inferred from keywords; steward should confirm or demote",
            f"airc queue heartbeat {card['ref']} --note \"Review priority: confirm P0 or set explicit P1/P2.\"",
            "info",
        ))

active_by_owner = defaultdict(list)
active_cards = [c for c in cards if c["status"] != "merged"]
for card in active_cards:
    if card["owner"] and card["status"] in ACTIVE_STATUSES:
        active_by_owner[card["owner"]].append(card)
for name, owned in active_by_owner.items():
    if len(owned) > 3:
        refs = ", ".join(c["ref"] for c in owned[:8])
        actions.append(action(
            "owner-overloaded",
            None,
            f"{name} owns {len(owned)} active cards: {refs}",
            f"airc queue availability {repo} --stale-after {stale_after_raw}",
            "warn",
        ))

priority_counts = Counter(c["priority"] for c in active_cards)
if active_cards and priority_counts.get("P0", 0) / len(active_cards) >= 0.75 and len(active_cards) >= 4:
    actions.append(action(
        "priority-collapse",
        None,
        f"{priority_counts.get('P0', 0)}/{len(active_cards)} active cards are P0; priority has lost meaning",
        f"airc queue steward {repo} --json",
        "warn",
    ))

summary = {
    "open": len(cards),
    "active": len(active_cards),
    "actions": len(actions),
    "stale": sum(1 for c in active_cards if c["stale_reason"]),
    "claimable": sum(1 for c in active_cards if c["status"] in {"claimed", "blocked"} and not c["owner"]),
    "priorities": dict(priority_counts),
}
payload = {
    "repo": repo,
    "owner": owner,
    "now_utc": now.isoformat().replace("+00:00", "Z"),
    "stale_after": stale_after_raw,
    "summary": summary,
    "actions": actions,
}

if output_json:
    print(json.dumps(payload, indent=2))
    sys.exit(0)

print(f"# airc queue steward — {repo}")
print(f"now_utc: {payload['now_utc']}")
print(f"owner: {owner}")
print(
    "summary: "
    f"open={summary['open']} "
    f"active={summary['active']} "
    f"actions={summary['actions']} "
    f"stale={summary['stale']} "
    f"claimable={summary['claimable']} "
    f"P0={summary['priorities'].get('P0', 0)} "
    f"P1={summary['priorities'].get('P1', 0)}"
)
print("\n## Proposed Steward Actions")
if not actions:
    print("- none")
else:
    for item in actions[:20]:
        target = f" {item['ref']}" if item["ref"] else ""
        print(f"- [{item['severity']}] {item['kind']}{target}")
        if item["title"]:
            print(f"  title: {short(item['title'])}")
        print(f"  reason: {short(item['reason'])}")
        print(f"  command: {item['command']}")
print("\nmode: dry-run/read-only; commands are recommendations, not automatic mutations")
PYEOF
  local py_status=$?
  rm -f "$raw_json_file"
  return "$py_status"
}
