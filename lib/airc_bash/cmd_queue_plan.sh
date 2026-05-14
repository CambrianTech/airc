# shellcheck shell=bash
# Sourced by cmd_queue.sh. Cohesive queue planning view.

_cmd_queue_plan() {
  # Print the prioritized kanban state for a repo. This is the "one command"
  # path for agents: see blockers, reviews, active ownership, stale work,
  # strategic lanes, and the next concrete actions without stitching
  # together list/stale/next/availability manually.
  local target_repo=""
  local limit=100
  local output_json=0
  local stale_after="30m"
  local owner=""

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_plan_help
        return 0
        ;;
      --repo)        shift; target_repo="${1:-}" ;;
      --limit)       shift; limit="${1:-100}" ;;
      --stale-after) shift; stale_after="${1:-30m}" ;;
      --owner)       shift; owner="${1:-}" ;;
      --json)        output_json=1 ;;
      -*) die "queue plan: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue plan: too many positional args (use: queue plan [owner/repo])"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    die "queue plan: no <owner/repo> given and could not detect one from \$PWD's git remote. Run inside a GitHub checkout or pass --repo owner/repo."
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue plan: target must be owner/repo, got: $target_repo" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue plan: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue plan: --limit must be >= 1 (got: $limit)"
  fi
  if [ -z "$owner" ]; then
    owner=$(_airc_queue_resolve_name)
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue plan: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,updatedAt,createdAt 2>&1); then
    die "queue plan: gh issue list failed for $target_repo: $raw_json"
  fi

  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-plan.XXXXXX") || die "queue plan: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  AIRC_QUEUE_PLAN_OWNER="$owner" \
  "$AIRC_PYTHON" - "$target_repo" "$stale_after" "$output_json" "$raw_json_file" <<'PYEOF'
import datetime as dt
import json
import os
import re
import sys
from collections import Counter, defaultdict

repo, stale_after_raw, output_json_raw, path = sys.argv[1:5]
output_json = output_json_raw == "1"
owner = os.environ.get("AIRC_QUEUE_PLAN_OWNER") or "anonymous"

CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)

LANES = [
    ("alpha-gap/rust-runtime", (
        "rust", "persona", "cognition", "ts-rs", "runtime", "node",
        "engram", "prompt", "evaluator", "provider", "model", "ai throughput",
    )),
    ("perf/resource-control", (
        "perf", "speed", "cpu", "memory", "gpu", "docker", "resource",
        "throughput", "latency", "capacity", "qwen", "cuda", "metal",
        "vulkan", "livekit", "webrtc", "framebuffer", "texture",
    )),
    ("flywheel/automation", (
        "airc", "queue", "kanban", "automation", "metronome", "nudge",
        "close-merged", "issue", "pr", "canary", "workflow", "precommit",
        "collaboration", "owner", "claim",
    )),
    ("quality/tests-vdd", (
        "test", "vdd", "tdd", "lint", "clippy", "baseline", "validation",
        "smoke", "roundtrip", "hygiene", "ratchet", "determinism",
    )),
    ("ui/configurator", (
        "ui", "browser", "widget", "configurator", "render", "web",
        "tsx", "react", "adapter",
    )),
    ("integration/canary", (
        "canary", "merge", "release", "install", "image", "main",
        "pre-push", "prepush", "ci",
    )),
]

P0_WORDS = (
    "broken", "failing", "fail", "regression", "panic", "crash", "blocked",
    "idle", "flywheel", "canary", "main", "merge", "stale", "wrong",
)
P1_WORDS = (
    "rust", "perf", "speed", "memory", "cpu", "gpu", "resource", "ts-rs",
    "test", "vdd", "tdd", "cleanup", "refactor", "docker", "qwen",
)

def parse_duration(value: str) -> dt.timedelta:
    match = re.fullmatch(r"\s*(\d+)\s*([smhd])\s*", value or "")
    if not match:
        print(f"queue plan: cannot parse --stale-after '{value}' (use 30m, 2h, 1d)", file=sys.stderr)
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

def lane_for(text: str) -> str:
    for lane, words in LANES:
        if any(word in text for word in words):
            return lane
    return "backlog/general"

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

def shquote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"

def short(value: str, limit: int = 96) -> str:
    value = " ".join((value or "").split())
    if len(value) <= limit:
        return value
    return value[: limit - 1].rstrip() + "..."

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
    body_text = text_for(issue, card)
    lane = lane_for(body_text)
    priority = priority_for(status, body_text, card, bool(stale_reason))
    ref = f"{repo}#{issue.get('number')}"
    cards.append({
        "number": issue.get("number"),
        "ref": ref,
        "title": compact_title(issue.get("title", "")),
        "url": issue.get("url") or "",
        "status": status,
        "owner": card_owner,
        "lane": lane,
        "priority": priority,
        "branch": (card.get("branch") or "").strip(),
        "env": (card.get("env") or "").strip(),
        "blockers": (card.get("blockers") or "").strip(),
        "evidence": (card.get("evidence") or "").strip(),
        "next_action": (card.get("next_action") or "").strip(),
        "last_heartbeat": heartbeat,
        "stale_reason": stale_reason,
        "updatedAt": issue.get("updatedAt") or "",
        "claim_command": f"airc queue claim {shquote(ref)} --owner {shquote(owner)}",
    })

priority_order = {"P0": 0, "P1": 1, "P2": 2, "P3": 3}
status_order = {"review": 0, "blocked": 1, "claimed": 2, "in-progress": 3, "merged": 4}
cards.sort(key=lambda c: (
    priority_order.get(c["priority"], 9),
    status_order.get(c["status"], 9),
    0 if not c["owner"] else 1,
    c["number"] or 0,
))

summary = {
    "open": len(cards),
    "priorities": dict(Counter(c["priority"] for c in cards)),
    "statuses": dict(Counter(c["status"] for c in cards)),
    "stale": sum(1 for c in cards if c["stale_reason"]),
    "owned": sum(1 for c in cards if c["owner"]),
    "unowned": sum(1 for c in cards if not c["owner"]),
}
lanes = defaultdict(list)
owners = defaultdict(list)
for card in cards:
    lanes[card["lane"]].append(card)
    if card["owner"]:
        owners[card["owner"]].append(card)

payload = {
    "repo": repo,
    "owner": owner,
    "now_utc": now.isoformat().replace("+00:00", "Z"),
    "stale_after": stale_after_raw,
    "summary": summary,
    "cards": cards,
    "lanes": {lane: [c["ref"] for c in rows] for lane, rows in lanes.items()},
    "owners": {name: [c["ref"] for c in rows] for name, rows in owners.items()},
}

if output_json:
    print(json.dumps(payload, indent=2))
    sys.exit(0)

def print_card(card: dict, prefix: str = "-") -> None:
    owner_label = card["owner"] or "unowned"
    bits = [card["ref"], f"[{card['priority']}]", f"[{card['status']}]", card["lane"], f"owner={owner_label}"]
    if card["stale_reason"]:
        bits.append(f"stale={card['stale_reason']}")
    print(f"{prefix} {' '.join(bits)}")
    print(f"  title: {short(card['title'])}")
    if card["next_action"]:
        print(f"  next:  {short(card['next_action'])}")
    if card["blockers"]:
        print(f"  blockers: {short(card['blockers'])}")

print(f"# airc queue plan — {repo}")
print(f"now_utc: {payload['now_utc']}")
print(f"owner: {owner}")
print(
    "summary: "
    f"open={summary['open']} "
    f"P0={summary['priorities'].get('P0', 0)} "
    f"P1={summary['priorities'].get('P1', 0)} "
    f"review={summary['statuses'].get('review', 0)} "
    f"blocked={summary['statuses'].get('blocked', 0)} "
    f"stale={summary['stale']} "
    f"unowned={summary['unowned']}"
)

if not cards:
    print("No open airc-queue cards.")
    print(f"next: airc queue add {repo} --title \"Describe the next concrete task\" --status claimed --owner unclaimed")
    sys.exit(0)

print("\n## P0 now")
p0_cards = [c for c in cards if c["priority"] == "P0"]
if p0_cards:
    for card in p0_cards[:8]:
        print_card(card)
else:
    print("- none")

print("\n## Review / merge candidates")
review_cards = [c for c in cards if c["status"] == "review"]
if review_cards:
    for card in review_cards[:8]:
        print_card(card)
else:
    print("- none")

print("\n## Active ownership")
if owners:
    for name in sorted(owners):
        owned = sorted(owners[name], key=lambda c: (priority_order.get(c["priority"], 9), c["number"] or 0))
        refs = ", ".join(f"{c['ref']}({c['status']}/{c['priority']})" for c in owned[:8])
        print(f"- {name}: {refs}")
else:
    print("- none")

print("\n## Stale / needs nudge")
stale_cards = [c for c in cards if c["stale_reason"]]
if stale_cards:
    for card in stale_cards[:8]:
        print_card(card)
else:
    print("- none")

print("\n## Strategic lanes")
lane_names = [lane for lane, _ in LANES] + ["backlog/general"]
for lane in lane_names:
    rows = lanes.get(lane, [])
    if not rows:
        continue
    examples = ", ".join(f"{c['ref']}({c['status']}/{c['priority']})" for c in rows[:6])
    print(f"- {lane}: {len(rows)} open — {examples}")

print("\n## Next actions")
actions = []
if review_cards:
    actions.append(f"Review/merge {review_cards[0]['ref']}: {short(review_cards[0]['title'], 72)}")
if stale_cards:
    actions.append(f"Nudge or release stale claim {stale_cards[0]['ref']}: {stale_cards[0]['stale_reason']}")
claimable = [c for c in cards if c["status"] in {"claimed", "blocked"} and not c["owner"]]
if claimable:
    actions.append(f"Claim top ready card: {claimable[0]['claim_command']}")
if not actions:
    actions.append("No obvious queue move; create a missing P1/P2 card from the next code smell or test gap.")
for idx, action in enumerate(actions[:5], start=1):
    print(f"{idx}. {action}")
PYEOF
  local py_status=$?
  rm -f "$raw_json_file"
  return "$py_status"
}
