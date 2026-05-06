"""channel_gist — find-or-create the canonical gist for a channel name on
the user's gh account.

Single concern: given a channel name (e.g. "general", "acme",
"example"), return the gist id that hosts that channel for THIS gh
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
import time
from datetime import datetime, timezone
from typing import Optional

from . import gh_backoff


_GH_BIN = "gh"
_GH_API_TIMEOUT = 10.0
_GIST_LIST_LIMIT = 100   # `airc list` uses 50; we go a bit higher to be safe
_GIST_ID_CHARS = set("0123456789abcdefABCDEF")
_LAST_GIST_LIST_UNAVAILABLE = False
def _cache_path(name: str) -> str:
    uid = str(os.getuid()) if hasattr(os, "getuid") else os.environ.get("USERNAME", "user")
    return os.path.join(tempfile.gettempdir(), f"airc-{name}-{uid}.json")


def _load_cached_gist_list(max_age: float) -> Optional[list[dict]]:
    path = _cache_path("gh-gist-list")
    try:
        age = time.time() - os.path.getmtime(path)
        if age > max_age:
            return None
        with open(path, encoding="utf-8") as f:
            loaded = json.load(f)
        if isinstance(loaded, list):
            return loaded
    except (OSError, ValueError, TypeError):
        return None
    return None


def _save_cached_gist_list(gists: list[dict]) -> None:
    path = _cache_path("gh-gist-list")
    tmp = f"{path}.{os.getpid()}.tmp"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(gists, f)
        os.replace(tmp, path)
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def _remember_created_gist(gist_id: str, channel: str, description: str, envelope: dict) -> None:
    """Update the local gist-list cache after creating a room gist.

    GitHub's list endpoint is eventually consistent. Without this, the
    same process can create a room gist, immediately bounce, read a
    still-stale cached list that lacks the new gist, and create a second
    duplicate room. The cache entry only needs the fields used by
    find_existing(): id, timestamps, description, and inline file content.
    """
    if not gist_id:
        return
    cached = _load_cached_gist_list(float("inf")) or []
    cached = [g for g in cached if g.get("id") != gist_id]
    now = datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    filename = f"airc-room-{channel}.json"
    cached.insert(0, {
        "id": gist_id,
        "description": description,
        "created_at": now,
        "updated_at": now,
        "files": {
            filename: {
                "filename": filename,
                "content": json.dumps(envelope),
            }
        },
    })
    _save_cached_gist_list(cached[:_GIST_LIST_LIMIT])


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

    Cached because this is the hottest GitHub control-plane path:
    join discovery, send-path recovery, and the rediscovery loop all
    call through here. One machine can run several agents on the same
    gh account, so "one list every 30s" quickly becomes enough traffic
    to trip secondary limits. Fresh cache default is 60s; if the live
    probe fails, stale cache default is 15m so peers can keep using the
    last-known room map instead of creating new islands.
    """
    global _LAST_GIST_LIST_UNAVAILABLE
    _LAST_GIST_LIST_UNAVAILABLE = False
    cache_sec = float(os.environ.get("AIRC_GIST_LIST_CACHE_SEC", "60"))
    stale_sec = float(os.environ.get("AIRC_GIST_LIST_STALE_SEC", "900"))
    cached = _load_cached_gist_list(cache_sec)
    if cached is not None:
        return cached
    if gh_backoff.backoff_active():
        _LAST_GIST_LIST_UNAVAILABLE = True
        return _load_cached_gist_list(stale_sec) or []

    gh = _resolve_gh_bin()
    if gh is None:
        _LAST_GIST_LIST_UNAVAILABLE = True
        return _load_cached_gist_list(stale_sec) or []
    try:
        r = gh_backoff.run_gh(
            gh,
            ["api", "--include", f"gists?per_page={_GIST_LIST_LIMIT}"],
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT * 3,
        )
    except (subprocess.TimeoutExpired, OSError):
        _LAST_GIST_LIST_UNAVAILABLE = True
        return _load_cached_gist_list(stale_sec) or []
    if r.returncode != 0:
        _LAST_GIST_LIST_UNAVAILABLE = True
        gh_backoff.record_backoff((r.stderr or "") + (r.stdout or ""))
        return _load_cached_gist_list(stale_sec) or []
    out: list[dict] = []
    headers, body = gh_backoff.split_include_output(r.stdout)
    gh_backoff.record_backoff(headers)
    raw = body.strip()
    if not raw:
        return []
    try:
        loaded = json.loads(raw)
        if isinstance(loaded, list):
            _save_cached_gist_list(loaded)
            return loaded
    except (ValueError, TypeError):
        pass
    _LAST_GIST_LIST_UNAVAILABLE = True
    return out


def _gist_describes_channel(gist: dict, channel: str, require_invite: bool = False) -> bool:
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
            if require_invite and not env.get("invite"):
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
    if gh_backoff.backoff_active():
        return None
    try:
        r = gh_backoff.run_gh(
            gh,
            ["api", "--include", f"gists/{gist_id}"],
            capture_output=True, text=True, timeout=_GH_API_TIMEOUT,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if r.returncode != 0:
        gh_backoff.record_backoff((r.stderr or "") + (r.stdout or ""))
        return None
    headers, body = gh_backoff.split_include_output(r.stdout)
    gh_backoff.record_backoff(headers)
    try:
        return json.loads(body)
    except (ValueError, TypeError):
        return None


def _valid_gist_id(gist_id: object) -> bool:
    if not isinstance(gist_id, str):
        return False
    return 8 <= len(gist_id) <= 64 and all(c in _GIST_ID_CHARS for c in gist_id)


def _config_channel_gist(config_path: Optional[str], channel: str) -> Optional[str]:
    if not config_path:
        return None
    try:
        with open(config_path, encoding="utf-8") as f:
            cfg = json.load(f)
    except (OSError, ValueError, TypeError):
        return None
    gists = cfg.get("channel_gists") or {}
    gid = gists.get(channel)
    return gid if _valid_gist_id(gid) else None


def _parse_ts(value: object) -> float:
    if not isinstance(value, str) or not value:
        return 0.0
    text = value.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        dt = datetime.fromisoformat(text)
    except ValueError:
        return 0.0
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.timestamp()


def _gist_activity_ts(gist: dict) -> float:
    """Best-effort freshness signal from cloned gist contents.

    GitHub REST `updated_at` is not available on the git fallback path.
    The wire data itself is enough: room envelopes carry heartbeat-ish
    timestamps and messages.jsonl lines carry message timestamps.
    """
    best = _parse_ts(gist.get("updated_at")) or _parse_ts(gist.get("created_at"))
    files = gist.get("files") or {}
    for name, entry in files.items():
        content = entry.get("content")
        if not isinstance(content, str) or not content:
            continue
        if name == "messages.jsonl":
            for line in content.splitlines():
                try:
                    env = json.loads(line)
                except (ValueError, TypeError):
                    continue
                if not isinstance(env, dict):
                    continue
                for key in ("ts", "updated", "last_heartbeat", "created"):
                    best = max(best, _parse_ts(env.get(key)))
            continue
        try:
            env = json.loads(content)
        except (ValueError, TypeError):
            continue
        if not isinstance(env, dict):
            continue
        for key in ("updated", "updated_at", "last_heartbeat", "created", "created_at"):
            best = max(best, _parse_ts(env.get(key)))
    return best


def _strict_single_channel_match(gist: dict, channel: str, require_invite: bool = False) -> bool:
    """True only when the envelope is exclusively for this channel.

    This is stricter than _is_single_channel_match, which deliberately
    tolerates older heartbeat code adding sibling labels to an exact
    filename. The local fallback uses this stricter tier first so a
    newer multi-channel solo island cannot beat a real per-channel
    chain during REST outage recovery.
    """
    files = gist.get("files") or {}
    exact_name = f"airc-room-{channel}.json"
    for name, entry in files.items():
        content = entry.get("content")
        if not content:
            continue
        try:
            env = json.loads(content)
        except (ValueError, TypeError):
            continue
        if not isinstance(env, dict):
            continue
        if require_invite and not env.get("invite"):
            continue
        channels = env.get("channels")
        if isinstance(channels, list) and channels == [channel]:
            return name == exact_name or len(files) == 1
    return False


def _local_config_paths() -> list[str]:
    raw = os.environ.get("AIRC_GIST_CACHE_ROOTS", "")
    paths: list[str] = []
    if raw:
        for entry in (p for p in raw.split(os.pathsep) if p):
            expanded = os.path.abspath(os.path.expanduser(entry))
            if os.path.isdir(expanded):
                paths.append(os.path.join(expanded, "config.json"))
            else:
                paths.append(expanded)
    airc_home = os.environ.get("AIRC_HOME", "")
    if airc_home:
        paths.append(os.path.join(os.path.abspath(airc_home), "config.json"))

    out: list[str] = []
    seen: set[str] = set()
    for path in paths:
        path = os.path.abspath(os.path.expanduser(path))
        if path in seen or not os.path.isfile(path):
            continue
        seen.add(path)
        out.append(path)
    return out


def _local_config_gist_candidates(channel: str) -> list[tuple[str, float]]:
    """Return locally remembered gist ids for channel across worktrees.

    This is deliberately read-only evidence. It lets a machine recover
    a previously-known canonical room while the gh REST listing surface
    is unavailable, without creating a new island. Default discovery is
    intentionally constrained to the current AIRC_HOME; broader scans
    are opt-in through AIRC_GIST_CACHE_ROOTS so background daemons do not
    crawl user folders or trip OS privacy prompts.
    """
    found: dict[str, float] = {}
    for path in _local_config_paths():
        try:
            with open(path, encoding="utf-8") as f:
                cfg = json.load(f)
        except (OSError, ValueError, TypeError):
            continue
        gists = cfg.get("channel_gists") or {}
        gid = gists.get(channel)
        if not _valid_gist_id(gid):
            continue
        try:
            mtime = os.path.getmtime(path)
        except OSError:
            mtime = 0.0
        found[gid] = max(found.get(gid, 0.0), mtime)
    return list(found.items())


def _git_gist_snapshot(gist_id: str) -> Optional[dict]:
    """Read a gist through the git endpoint, bypassing gh REST listing.

    `git clone https://gist.github.com/<id>.git` has been reachable in
    the same failures where `gh api /gists` is throttled or confused by
    invalid env auth. We only use it for candidate ids already found in
    local config, not for global discovery.
    """
    if not _valid_gist_id(gist_id) or shutil.which("git") is None:
        return None
    tmpdir = tempfile.mkdtemp(prefix="airc-gist-snapshot-")
    try:
        r = subprocess.run(
            [
                "git",
                "-c",
                "credential.helper=",
                "clone",
                "--quiet",
                "--depth",
                "1",
                f"https://gist.github.com/{gist_id}.git",
                tmpdir,
            ],
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT,
            env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
        )
        if r.returncode != 0:
            return None
        files: dict[str, dict] = {}
        for name in os.listdir(tmpdir):
            path = os.path.join(tmpdir, name)
            if name == ".git" or not os.path.isfile(path):
                continue
            try:
                with open(path, encoding="utf-8") as f:
                    files[name] = {"content": f.read(), "truncated": False}
            except OSError:
                continue
        if not files:
            return None
        desc = "airc mesh"
        for name in files:
            if name.startswith("airc-room-") and name.endswith(".json"):
                room = name[len("airc-room-"):-len(".json")]
                desc = f"airc room: #{room} (git fallback)"
                break
        return {"id": gist_id, "description": desc, "files": files}
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


def _choose_local_fallback(matches: list[tuple[dict, float]], channel: str, require_invite: bool) -> Optional[str]:
    if not matches:
        return None

    def rank(item: tuple[dict, float]) -> tuple[int, float, float, str]:
        gist, local_mtime = item
        if _strict_single_channel_match(gist, channel, require_invite=require_invite):
            tier = 3
        elif _is_single_channel_match(gist, channel, require_invite=require_invite):
            tier = 2
        elif _gist_describes_channel(gist, channel, require_invite=require_invite):
            tier = 1
        else:
            tier = 0
        return (tier, _gist_activity_ts(gist), local_mtime, gist.get("id", ""))

    valid = [item for item in matches if rank(item)[0] > 0]
    if not valid:
        return None
    valid.sort(key=rank, reverse=True)
    return valid[0][0].get("id")


def _find_existing_via_local_cache(channel: str, require_invite: bool = False) -> Optional[str]:
    if os.environ.get("AIRC_DISABLE_LOCAL_GIST_FALLBACK") == "1":
        return None
    snapshots: list[tuple[dict, float]] = []
    for gid, local_mtime in _local_config_gist_candidates(channel):
        snapshot = _git_gist_snapshot(gid)
        if snapshot is None:
            continue
        snapshots.append((snapshot, local_mtime))
    return _choose_local_fallback(snapshots, channel, require_invite)


def _is_single_channel_match(gist: dict, channel: str, require_invite: bool = False) -> bool:
    """Return True for the canonical post-3c per-channel gist.

    The canonical signal is the in-gist filename
    `airc-room-<channel>.json`. Older hosts briefly wrote multiple
    channel labels into that envelope during heartbeat refresh; do not
    let that demote the actual per-channel chain below a newer solo
    invite duplicate.
    """
    files = gist.get("files") or {}
    exact_name = f"airc-room-{channel}.json"
    for name, entry in files.items():
        content = entry.get("content")
        if not content:
            continue
        try:
            env = json.loads(content)
        except (ValueError, TypeError):
            continue
        if not isinstance(env, dict):
            continue
        if require_invite and not env.get("invite"):
            continue
        channels = env.get("channels")
        if name == exact_name and isinstance(channels, list) and channel in channels:
            return True
        if isinstance(channels, list) and len(channels) == 1 and channels[0] == channel:
            return True
    return False


def find_existing(channel: str, require_invite: bool = False) -> Optional[str]:
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
    DIFFERENT duplicates, splitting the substrate. Two agents saw this
    on #general — peers thought they were
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

    def _room_desc_matches(matches: list[dict]) -> list[dict]:
        """Prefer explicit per-channel room gists over generic mesh gists.

        A bad host can add `airc-room-<channel>.json` to a newer generic
        `airc mesh` gist during recovery. The durable chain created by
        channel_gist.create_new has description `airc room: #<channel>`;
        that description is a stronger canonical signal than a generic
        mesh description with an accidentally matching filename.
        """
        prefix = f"airc room: #{channel}"
        return [g for g in matches if (g.get("description") or "").strip().startswith(prefix)]

    def _choose_canonical(matches: list[dict]) -> Optional[str]:
        exact_desc = _room_desc_matches(matches)
        return _oldest(exact_desc) or _oldest(matches)

    # Pass 1: canonical single-channel match (cheap, listing-response).
    canonical_matches = [g for g in candidates if _is_single_channel_match(g, channel, require_invite=require_invite)]
    chosen = _choose_canonical(canonical_matches)
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
        if _is_single_channel_match(full, channel, require_invite=require_invite):
            # Carry created_at from the listing so _oldest can sort.
            full.setdefault("created_at", g.get("created_at", ""))
            deep_canonical.append(full)
    chosen = _choose_canonical(deep_canonical)
    if chosen:
        return chosen

    # Pass 2: legacy multi-channel fallback. Only if no canonical exists.
    legacy_matches = [g for g in candidates if _gist_describes_channel(g, channel, require_invite=require_invite)]
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
        if _gist_describes_channel(full, channel, require_invite=require_invite):
            full.setdefault("created_at", g.get("created_at", ""))
            deep_legacy.append(full)
    chosen = _oldest(deep_legacy)
    if chosen:
        return chosen

    return _find_existing_via_local_cache(channel, require_invite=require_invite)


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
            r = gh_backoff.run_gh(
                gh,
                ["gist", "create", "-d", desc, seed_path],
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
        gist_id = last.rsplit("/", 1)[-1] or None
        if gist_id:
            _remember_created_gist(gist_id, channel, desc, envelope)
        return gist_id
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


def resolve(channel: str, create_if_missing: bool = False, require_invite: bool = False) -> Optional[str]:
    """Resolve a channel name to its gist id on this gh account.

    Returns the gist id on success (existing or newly-created), None
    if the channel can't be resolved (no gh auth, no existing gist
    AND create_if_missing=False, or creation failed).

    Two-step: find_existing then optionally create_new.

    Retry on miss: gh's gist listing has eventual consistency — a
    just-created gist may not appear in `gh gist list` for several
    seconds. Without retry, a peer who reconnects right after another
    peer hosted misses the canonical and creates a duplicate. Retry
    twice with backoff before giving up; bounded so create_if_missing
    callers don't wait forever on a genuinely-empty account.
    """
    if not channel or not isinstance(channel, str):
        return None
    import os as _os
    import time as _t
    # AIRC_RESOLVE_NO_RETRY: callers that DON'T want to wait on gh's
    # listing-consistency lag (host-bootstrap find-first — if no gist
    # exists, we'll create one anyway, no point waiting). Joiner paths
    # leave it unset so they retry through the propagation window.
    no_retry = _os.environ.get("AIRC_RESOLVE_NO_RETRY") == "1"
    attempts = 1 if no_retry else 3
    for attempt in range(attempts):
        existing = find_existing(channel, require_invite=require_invite)
        if existing:
            return existing
        if attempt < attempts - 1:
            _t.sleep(1.5 * (attempt + 1))  # 1.5s, then 3s
    if create_if_missing and not require_invite:
        if gh_backoff.backoff_active() or _LAST_GIST_LIST_UNAVAILABLE:
            return None
        return create_new(channel)
    return None


def host_preflight(channel: str, config_path: Optional[str] = None) -> tuple[str, Optional[str]]:
    """Return the host bootstrap decision for a channel.

    - ("existing", gid): use this canonical gist.
    - ("blocked", None): discovery was unavailable; do not create.
    - ("create", None): discovery was trusted and no gist exists.

    Host bootstrap writes a richer invite envelope than create_new(), so
    bash still owns the actual gist create. This helper owns the safety
    decision so a failed GitHub listing cannot be mistaken for an empty
    account.
    """
    configured = _config_channel_gist(config_path, channel)
    if configured:
        return "existing", configured
    existing = find_existing(channel)
    if existing:
        return "existing", existing
    if gh_backoff.backoff_active() or _LAST_GIST_LIST_UNAVAILABLE:
        return "blocked", None
    return "create", None


# ── CLI entry — bash invokes this from cmd_connect / cmd_subscribe ──

def _cli() -> int:
    import argparse
    parser = argparse.ArgumentParser(prog="airc_core.channel_gist")
    sub = parser.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("resolve", help="Print gist id for a channel; empty stdout if absent")
    r.add_argument("--channel", required=True)
    r.add_argument("--create-if-missing", action="store_true")
    r.add_argument("--require-invite", action="store_true")

    f = sub.add_parser("find", help="Find existing only (no create)")
    f.add_argument("--channel", required=True)
    f.add_argument("--require-invite", action="store_true")

    hp = sub.add_parser("host-preflight", help="Print existing gist id, or exit 2 when discovery is unavailable")
    hp.add_argument("--channel", required=True)
    hp.add_argument("--config", default="")

    m = sub.add_parser("remember-created", help="Record a just-created room gist in the local discovery cache")
    m.add_argument("--channel", required=True)
    m.add_argument("--gist-id", required=True)
    m.add_argument("--description", required=True)
    m.add_argument("--payload-file", required=True)

    args = parser.parse_args()

    if args.cmd == "resolve":
        gid = resolve(args.channel, create_if_missing=args.create_if_missing, require_invite=args.require_invite)
        if gid:
            print(gid)
            return 0
        return 1
    if args.cmd == "find":
        gid = find_existing(args.channel, require_invite=args.require_invite)
        if gid:
            print(gid)
            return 0
        return 1
    if args.cmd == "host-preflight":
        decision, gid = host_preflight(args.channel, config_path=args.config)
        if decision == "existing" and gid:
            print(gid)
            return 0
        if decision == "blocked":
            return 2
        return 1
    if args.cmd == "remember-created":
        try:
            with open(args.payload_file, encoding="utf-8") as f:
                envelope = json.load(f)
        except (OSError, ValueError, TypeError):
            return 1
        _remember_created_gist(args.gist_id, args.channel, args.description, envelope)
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(_cli())
