"""channel_gist — find-or-create the canonical gist for a channel name on
the user's gh account.

Single concern: given a channel name (e.g. "general", "useideem",
"continuum"), return the gist id that hosts that channel for THIS gh
account. If no such gist exists and create_if_missing=True, publish a
new mesh-shaped gist and return its id.

This is the ONE place in the codebase that knows "channel name → gist
id" lookup. cmd_connect uses it to bootstrap each subscribed channel;
cmd_send uses it via airc_core.config to route by channel.

Architecture (per Joel 2026-04-29): one gist per channel name per gh
account. ALL of my tabs/machines that subscribe to #general read+write
the same #general gist. Closing/opening tabs is benign — the gist
persists. New peers joining a channel just discover the existing gist.

Why a separate module rather than a function in cmd_rooms.sh: cmd_rooms
already enumerates gh gists for the `airc list` UI surface, but its
parse logic is wired into bash heredocs and shaped for human display
(mnemonic, descriptions, kind tags). Extracting that for the
machine-driven find-or-create here means a fresh, focused module
instead of bash-flavored gymnastics. Same-shape concern as bearer_*:
small, single-purpose, callable from both python and bash via a CLI
seam.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from typing import Optional


_GH_BIN = "gh"
_GH_API_TIMEOUT = 10.0
_GIST_LIST_LIMIT = 100   # `airc list` uses 50; we go a bit higher to be safe


def _resolve_gh_bin() -> Optional[str]:
    """Return path to gh CLI, or None if absent. Caller-visible None
    means we can't do gh-side resolution at all — return early."""
    return shutil.which(_GH_BIN)


def _gh_list_user_gists() -> list[dict]:
    """List ALL of the authenticated user's gists, returning parsed
    JSON. Empty list on any failure (no gh auth, network blip, etc).

    Uses `gh api gists` (with pagination) rather than `gh gist list`
    because the JSON shape is stable + complete. gh gist list output
    is human-shaped (tab-delimited) and shifts across versions.
    """
    gh = _resolve_gh_bin()
    if gh is None:
        return []
    try:
        r = subprocess.run(
            [gh, "api", f"gists?per_page={_GIST_LIST_LIMIT}", "--paginate"],
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT * 3,
        )
    except (subprocess.TimeoutExpired, OSError):
        return []
    if r.returncode != 0:
        return []
    out: list[dict] = []
    # `--paginate` concatenates JSON arrays inline. Split on `][` to
    # rejoin pages without depending on gh's exact output shape.
    raw = r.stdout.strip()
    if not raw:
        return []
    chunks = raw.replace("][", "],[")
    if chunks.startswith("["):
        chunks = chunks
    try:
        # Try direct parse first (single-page or already-merged).
        loaded = json.loads(raw)
        if isinstance(loaded, list):
            return loaded
    except (ValueError, TypeError):
        pass
    # Fallback: split into separate JSON arrays and merge.
    try:
        joined = "[" + chunks.lstrip("[").rstrip("]") + "]"
        loaded = json.loads(joined)
        if isinstance(loaded, list):
            return loaded
    except (ValueError, TypeError):
        pass
    return out


def _gist_describes_channel(gist: dict, channel: str) -> bool:
    """Decide whether a gist envelope is THIS channel's room gist.

    A gist is the channel's room iff:
      1. Its description starts with airc's room marker "airc mesh" or
         "airc room:"
      2. AND its messages.jsonl-style content (or the legacy room
         envelope) lists this channel.

    We tolerate two shapes:
      - kind:"mesh"   with channels:["general", ...]   (Phase 2B+)
      - kind:"room"   with channels:["general", ...]   (legacy)

    The description field alone isn't enough — gh users may have
    unrelated gists named "airc test" etc. The envelope JSON is the
    authoritative signal.
    """
    desc = (gist.get("description") or "").strip()
    if not desc:
        return False
    # Cheap pre-filter so we don't fetch+parse every gist on the account.
    if not (desc.startswith("airc mesh") or desc.startswith("airc room:")):
        return False
    # Examine the gist's first file content (gist.files is a dict; pick
    # any value's "content" field — short ones are inlined in the
    # listing response, longer ones need a full GET).
    files = gist.get("files") or {}
    for entry in files.values():
        content = entry.get("content")
        if content:
            try:
                env = json.loads(content)
            except (ValueError, TypeError):
                continue
            if not isinstance(env, dict):
                continue
            channels = env.get("channels")
            if isinstance(channels, list) and channel in channels:
                return True
    return False


def _gh_api_get_gist(gist_id: str) -> Optional[dict]:
    """Full GET on a gist. Used when the listing response truncated
    file content and we need the full envelope to confirm a channel
    match."""
    gh = _resolve_gh_bin()
    if gh is None:
        return None
    try:
        r = subprocess.run(
            [gh, "api", f"gists/{gist_id}"],
            capture_output=True, text=True, timeout=_GH_API_TIMEOUT,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if r.returncode != 0:
        return None
    try:
        return json.loads(r.stdout)
    except (ValueError, TypeError):
        return None


def _is_single_channel_match(gist: dict, channel: str) -> bool:
    """A gist is the canonical post-3c per-channel gist for `channel`
    iff its envelope has channels=[<exactly channel>]. Single-element
    list, exact match. The post-3c shape created by create_new()."""
    files = gist.get("files") or {}
    for entry in files.values():
        content = entry.get("content")
        if not content:
            continue
        try:
            env = json.loads(content)
        except (ValueError, TypeError):
            continue
        if not isinstance(env, dict):
            continue
        channels = env.get("channels")
        if isinstance(channels, list) and len(channels) == 1 and channels[0] == channel:
            return True
    return False


def find_existing(channel: str) -> Optional[str]:
    """Look for an existing gist on this gh account hosting `channel`.
    Returns the gist id, or None if no match.

    Convergence rule: when multiple gists match (duplicates from
    repeated host-takeovers / race-loser collisions / orphaned
    re-creates), return the OLDEST by created_at. Deterministic
    across ALL peers on the gh account → all peers converge on the
    same gist → substrate is unified.

    Pre-2026-04-29 bug: order was whatever gh's list-response yielded
    first (recency-ordered, may differ across calls). Two peers
    polling the listing at slightly different times could pick
    DIFFERENT duplicates, splitting the substrate. authenticator-448f
    + continuum-b741 saw this on #general — peers thought they were
    in the same room but were writing to different gists. Sends
    looked successful, peers heard nothing.

    Two-pass to fix #290 (canonical-vs-legacy split): single-channel
    gists (channels=[<channel>] exactly) take priority over multi-
    channel mesh gists (channels=[a,b,c]). Within each pass, oldest
    wins for convergence.

    Each pass first checks the cheap listing-response content, then
    falls back to a full GET when the listing didn't inline content.
    """
    gists = _gh_list_user_gists()
    candidates: list[dict] = []
    for g in gists:
        desc = (g.get("description") or "").strip()
        if desc.startswith("airc mesh") or desc.startswith("airc room:"):
            candidates.append(g)

    def _oldest(matches: list[dict]) -> Optional[str]:
        """Return the gist id of the oldest match by created_at, or None."""
        if not matches:
            return None
        # gh's created_at is ISO-8601 ('2026-04-29T07:11:00Z') so
        # lexicographic sort matches chronological. Empty/missing
        # values sort first (treated as oldest), which is the safe
        # bias when in doubt.
        matches.sort(key=lambda g: g.get("created_at", ""))
        return matches[0].get("id")

    # Pass 1: canonical single-channel match (cheap, listing-response).
    canonical_matches = [g for g in candidates if _is_single_channel_match(g, channel)]
    chosen = _oldest(canonical_matches)
    if chosen:
        return chosen

    # Pass 1 (deep): full GET for each candidate whose listing-content
    # was truncated. Same single-channel criterion.
    deep_canonical: list[dict] = []
    for g in candidates:
        gid = g.get("id")
        if not gid:
            continue
        full = _gh_api_get_gist(gid)
        if full is None:
            continue
        if _is_single_channel_match(full, channel):
            # Carry created_at from the listing so _oldest can sort.
            full.setdefault("created_at", g.get("created_at", ""))
            deep_canonical.append(full)
    chosen = _oldest(deep_canonical)
    if chosen:
        return chosen

    # Pass 2: legacy multi-channel fallback. Only if no canonical exists.
    legacy_matches = [g for g in candidates if _gist_describes_channel(g, channel)]
    chosen = _oldest(legacy_matches)
    if chosen:
        return chosen

    deep_legacy: list[dict] = []
    for g in candidates:
        gid = g.get("id")
        if not gid:
            continue
        full = _gh_api_get_gist(gid)
        if full is None:
            continue
        if _gist_describes_channel(full, channel):
            full.setdefault("created_at", g.get("created_at", ""))
            deep_legacy.append(full)
    return _oldest(deep_legacy)


def create_new(channel: str) -> Optional[str]:
    """Publish a new mesh-shaped gist for `channel`. Returns gist id
    on success, None on failure.

    The gist envelope is minimal — channel name + airc version + a
    placeholder seed file (gh refuses gists with truly-empty files).
    The first send to the gist via GhBearer.send populates messages.jsonl.

    This is intentionally simpler than cmd_connect's full host-bootstrap
    envelope (which carries machine_id + addresses[] for TCP
    pair-handshake). After Phase 3c the TCP handshake is gone for
    cross-network peers — gh-as-bearer is the wire — so the rich
    addresses payload isn't needed for channel-only routing.
    """
    gh = _resolve_gh_bin()
    if gh is None:
        return None
    envelope = {
        "airc": 1,
        "kind": "mesh",
        "channels": [channel],
    }
    tmpdir = tempfile.mkdtemp(prefix="airc-channel-gist-")
    try:
        # Use airc-invite.<channel> as the seed filename; it's distinct
        # from messages.jsonl (which the bearer creates on first send),
        # so we don't shadow the wire file.
        seed_path = os.path.join(tmpdir, f"airc-room-{channel}.json")
        with open(seed_path, "w") as f:
            json.dump(envelope, f)
        desc = f"airc room: #{channel} (post-3c per-channel gist)"
        try:
            r = subprocess.run(
                [gh, "gist", "create", "-d", desc, seed_path],
                capture_output=True, text=True, timeout=_GH_API_TIMEOUT,
            )
        except (subprocess.TimeoutExpired, OSError):
            return None
        if r.returncode != 0:
            return None
        # Last URL line is the gist URL: https://gist.github.com/<user>/<id>
        last = r.stdout.strip().splitlines()[-1] if r.stdout.strip() else ""
        if not last:
            return None
        return last.rsplit("/", 1)[-1] or None
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


def resolve(channel: str, create_if_missing: bool = False) -> Optional[str]:
    """Resolve a channel name to its gist id on this gh account.

    Returns the gist id on success (existing or newly-created), None
    if the channel can't be resolved (no gh auth, no existing gist
    AND create_if_missing=False, or creation failed).

    Two-step: find_existing then optionally create_new. Keeps create
    opt-in so callers that just want to look up don't accidentally
    publish stray gists.
    """
    if not channel or not isinstance(channel, str):
        return None
    existing = find_existing(channel)
    if existing:
        return existing
    if create_if_missing:
        return create_new(channel)
    return None


# ── CLI entry — bash invokes this from cmd_connect / cmd_subscribe ──

def _cli() -> int:
    import argparse
    parser = argparse.ArgumentParser(prog="airc_core.channel_gist")
    sub = parser.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("resolve", help="Print gist id for a channel; empty stdout if absent")
    r.add_argument("--channel", required=True)
    r.add_argument("--create-if-missing", action="store_true")

    f = sub.add_parser("find", help="Find existing only (no create)")
    f.add_argument("--channel", required=True)

    args = parser.parse_args()

    if args.cmd == "resolve":
        gid = resolve(args.channel, create_if_missing=args.create_if_missing)
        if gid:
            print(gid)
            return 0
        return 1
    if args.cmd == "find":
        gid = find_existing(args.channel)
        if gid:
            print(gid)
            return 0
        return 1
    return 1


if __name__ == "__main__":
    sys.exit(_cli())
