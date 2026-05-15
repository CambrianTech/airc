# Sourced by airc. cmd_hygiene — workspace/cache policy for many-agent lanes.

cmd_hygiene() {
  local subcmd="${1:-report}"
  case "$subcmd" in
    -h|--help)
      _airc_hygiene_help
      return 0
      ;;
    init|report|clean)
      "$AIRC_PYTHON" -m airc_core.hygiene "$@"
      ;;
    *)
      die "hygiene: unknown subcommand: $subcmd (try: init, report, clean)"
      ;;
  esac
}

_airc_hygiene_help() {
  cat <<'EOF'
airc hygiene — project workspace/cache hygiene for multi-agent lanes

USAGE
  airc hygiene init [--force]
  airc hygiene report [--json]
  airc hygiene clean --dry-run
  airc hygiene clean --yes

POLICY
  Default policy path is <repo>/.airc-policy.json. It is intentionally
  non-secret and serde-friendly so the Rust AIRC rewrite can keep the same
  shape. Runtime identity/mesh state stays in private .airc/config.json.

RESOURCE REPORTING
  report includes disk, CPU load, memory availability, GPU hook status, and
  optional policy.report_paths. This is the manual surface that future
  automatic triggers can call from lane create/remove, queue metronome, doctor,
  and low-resource monitors.

SAFE CLEANUP
  By default, clean only removes rebuildable per-lane caches:
    - ~/.airc-worktrees/*/src/workers/target
    - ~/.airc-worktrees/*/src/node_modules

  Main checkout caches and Docker prune are policy-gated and off by default.
EOF
}
