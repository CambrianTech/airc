# Sourced by airc. cmd_lane — git worktree lanes for multi-agent safety.
#
# Lanes are local checkout isolation: one branch/worktree per agent card.
# Queue owns the task state; lane owns where an agent is allowed to edit.

cmd_lane() {
  local subcmd="${1:-}"
  shift || true

  case "$subcmd" in
    -h|--help|"")
      _airc_lane_help
      [ -z "$subcmd" ] && return 1
      return 0
      ;;
    create|new)
      _cmd_lane_create "$@"
      ;;
    list|ls)
      _cmd_lane_list "$@"
      ;;
    remove|rm)
      _cmd_lane_remove "$@"
      ;;
    *)
      die "lane: unknown subcommand: $subcmd (try: create, list, remove)"
      ;;
  esac
}

_cmd_lane_create() {
  local issue_ref=""
  local repo_root=""
  local branch=""
  local base="canary"
  local lane_dir=""
  local owner=""
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_lane_create_help
        return 0
        ;;
      --repo)    shift; repo_root="${1:-}" ;;
      --branch)  shift; branch="${1:-}" ;;
      --base)    shift; base="${1:-}" ;;
      --dir)     shift; lane_dir="${1:-}" ;;
      --owner|--agent) shift; owner="${1:-}" ;;
      --dry-run) dry_run=1 ;;
      -*) die "lane create: unknown flag: $1" ;;
      *)
        if [ -z "$issue_ref" ]; then
          issue_ref="$1"
        else
          die "lane create: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_ref" ]; then
    _airc_lane_create_help >&2
    return 1
  fi

  if [ -z "$repo_root" ]; then
    if ! repo_root=$(git rev-parse --show-toplevel 2>/dev/null); then
      die "lane create: not inside a git repo; pass --repo /path/to/repo"
    fi
  fi
  repo_root=$(_airc_lane_abs_path "$repo_root")
  [ -d "$repo_root/.git" ] || [ -f "$repo_root/.git" ] || die "lane create: --repo is not a git checkout: $repo_root"

  if [ -z "$owner" ]; then
    owner=$(_airc_queue_resolve_name 2>/dev/null || echo "anonymous")
  fi

  local repo_name issue_token owner_token
  repo_name=$(basename "$repo_root")
  issue_token=$(_airc_lane_slug "$issue_ref")
  owner_token=$(_airc_lane_slug "$owner")

  if [ -z "$branch" ]; then
    branch="feat/${issue_token}-${owner_token}"
  fi

  if [ -z "$lane_dir" ]; then
    lane_dir="$HOME/.airc/worktrees/${repo_name}-${issue_token}-${owner_token}"
  fi
  lane_dir=$(_airc_lane_abs_path "$lane_dir")

  case "$lane_dir" in
    "$repo_root"|"$repo_root"/*)
      die "lane create: refusing to create a lane inside the protected checkout: $lane_dir"
      ;;
  esac

  local resolved_base
  resolved_base=$(_airc_lane_resolve_base "$repo_root" "$base")

  if [ "$dry_run" -eq 1 ]; then
    printf 'DRY RUN — would create lane:\n'
    printf '  issue:  %s\n' "$issue_ref"
    printf '  repo:   %s\n' "$repo_root"
    printf '  dir:    %s\n' "$lane_dir"
    printf '  branch: %s\n' "$branch"
    printf '  base:   %s\n' "$resolved_base"
    printf '  owner:  %s\n' "$owner"
    return 0
  fi

  mkdir -p "$(dirname "$lane_dir")"
  if [ -e "$lane_dir" ]; then
    die "lane create: target dir already exists: $lane_dir"
  fi

  if git -C "$repo_root" show-ref --verify --quiet "refs/heads/$branch"; then
    git -C "$repo_root" worktree add "$lane_dir" "$branch"
  else
    git -C "$repo_root" worktree add -b "$branch" "$lane_dir" "$resolved_base"
  fi

  # Hydrate submodules in the new lane (continuum#1252).
  #
  # `git worktree add` does NOT init submodules in the new working tree,
  # so any repo with submodule deps (continuum has `vendor/llama.cpp` ~500MB)
  # has empty submodule directories that fail Rust precommit/prepush hooks
  # on first commit attempt with cryptic CMake errors. This step makes the
  # lane self-sufficient: agents shouldn't have to learn the
  # `git submodule update --init` ritual after every `airc lane create`.
  #
  # Fail loud (no `2>/dev/null`, no `|| true`): if submodule init fails the
  # lane is broken anyway — the user needs to see the error, not have it
  # swallowed. Repos with no submodules: this is a no-op and exits 0.
  git -C "$lane_dir" submodule update --init --recursive

  _airc_lane_record "$issue_ref" "$repo_root" "$lane_dir" "$branch" "$resolved_base" "$owner"

  printf 'Lane created:\n'
  printf '  issue:  %s\n' "$issue_ref"
  printf '  dir:    %s\n' "$lane_dir"
  printf '  branch: %s\n' "$branch"
  printf '  base:   %s\n' "$resolved_base"
}

_cmd_lane_list() {
  local output_json=0
  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_lane_list_help
        return 0
        ;;
      --json) output_json=1 ;;
      -*) die "lane list: unknown flag: $1" ;;
      *) die "lane list: unexpected arg: $1" ;;
    esac
    shift || true
  done

  local registry
  registry=$(_airc_lane_registry)
  mkdir -p "$(dirname "$registry")"
  touch "$registry"

  if [ "$output_json" -eq 1 ]; then
    "$(airc_rs_bin)" worktree-lane list --registry "$registry" --json
    return
  fi

  "$(airc_rs_bin)" worktree-lane list --registry "$registry"
}

_cmd_lane_remove() {
  local target=""
  local force=0
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_lane_remove_help
        return 0
        ;;
      --force) force=1 ;;
      --dry-run) dry_run=1 ;;
      -*) die "lane remove: unknown flag: $1" ;;
      *)
        if [ -z "$target" ]; then
          target="$1"
        else
          die "lane remove: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  [ -n "$target" ] || die "lane remove: pass a lane dir or issue ref"

  local registry
  registry=$(_airc_lane_registry)
  "$(airc_rs_bin)" worktree-lane find --registry "$registry" "$target" >/dev/null \
    || die "lane remove: no recorded lane matches: $target"

  local repo_root lane_dir
  repo_root=$("$(airc_rs_bin)" worktree-lane find --registry "$registry" "$target" --field repo)
  lane_dir=$("$(airc_rs_bin)" worktree-lane find --registry "$registry" "$target" --field dir)

  case "$lane_dir" in
    "$repo_root"|"$repo_root"/*)
      die "lane remove: refusing to remove protected checkout path: $lane_dir"
      ;;
  esac

  if [ "$dry_run" -eq 1 ]; then
    printf 'DRY RUN — would remove lane: %s\n' "$lane_dir"
    return 0
  fi

  if [ "$force" -eq 1 ]; then
    git -C "$repo_root" worktree remove --force "$lane_dir"
  else
    git -C "$repo_root" worktree remove "$lane_dir"
  fi
  printf 'Lane removed: %s\n' "$lane_dir"
}

_airc_lane_registry() {
  printf '%s/lanes.jsonl' "$AIRC_WRITE_DIR"
}

_airc_lane_abs_path() {
  "$(airc_rs_bin)" worktree-lane abs-path "$1"
}

_airc_lane_slug() {
  "$(airc_rs_bin)" worktree-lane slug "$1"
}

_airc_lane_resolve_base() {
  local repo_root="$1"
  local base="$2"
  if git -C "$repo_root" rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
    printf '%s' "$base"
  elif git -C "$repo_root" rev-parse --verify --quiet "origin/$base^{commit}" >/dev/null; then
    printf 'origin/%s' "$base"
  else
    die "lane create: base '$base' not found locally; fetch canary first"
  fi
}

_airc_lane_record() {
  local issue_ref="$1" repo_root="$2" lane_dir="$3" branch="$4" base="$5" owner="$6"
  local registry
  registry=$(_airc_lane_registry)
  mkdir -p "$(dirname "$registry")"
  "$(airc_rs_bin)" worktree-lane record \
    --registry "$registry" \
    --issue "$issue_ref" \
    --repo "$repo_root" \
    --dir "$lane_dir" \
    --branch "$branch" \
    --base "$base" \
    --owner "$owner"
}

_airc_lane_help() {
  cat <<'EOF'
airc lane — isolated git worktree lanes for multi-agent collaboration

USAGE
  airc lane create <issue-ref> [--repo PATH] [--branch NAME] [--base canary] [--dir PATH]
  airc lane list [--json]
  airc lane remove <issue-ref|dir> [--force]

DESCRIPTION
  Creates one git worktree per agent/card so agents do not edit the human's
  protected checkout or switch each other's branches. Default base is canary.
EOF
}

_airc_lane_create_help() {
  cat <<'EOF'
airc lane create — create an isolated worktree lane

USAGE
  airc lane create <issue-ref> [--repo PATH] [--branch NAME] [--base canary] [--dir PATH] [--owner HANDLE] [--dry-run]

OPTIONS
  --repo PATH       Source checkout. Default: current git repo.
  --branch NAME     Lane branch. Default: feat/<issue>-<owner>.
  --base NAME       Branch/ref to base from. Default: canary.
  --dir PATH        Worktree directory. Default: ~/.airc/worktrees/<repo>-<issue>-<owner>.
  --owner HANDLE    Lane owner. Default: current AIRC identity.
  --dry-run         Print without creating the worktree.
EOF
}

_airc_lane_list_help() {
  cat <<'EOF'
airc lane list — show recorded worktree lanes

USAGE
  airc lane list [--json]
EOF
}

_airc_lane_remove_help() {
  cat <<'EOF'
airc lane remove — remove a recorded worktree lane

USAGE
  airc lane remove <issue-ref|dir> [--force] [--dry-run]
EOF
}
