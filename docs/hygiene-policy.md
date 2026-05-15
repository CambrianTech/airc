# AIRC Hygiene Policy

`airc hygiene` uses a project policy file to keep many-agent workspaces from
filling a machine. The default path is:

```text
<repo>/.airc-policy.json
```

The file is optional. If it does not exist, AIRC uses built-in defaults. Run
`airc hygiene init` to write the default file for a project.

The policy is intentionally JSON and non-secret so the same shape can move to
Rust with `serde` without changing the command contract.

## Default Policy

```json
{
  "block_free_gb": 15.0,
  "clean_docker_build_cache": false,
  "clean_main_rust_target": false,
  "clean_worktree_node_modules": true,
  "clean_worktree_rust_targets": true,
  "hooks": [],
  "report_paths": [],
  "warn_free_gb": 50.0,
  "workspace_root": "~/.airc-worktrees"
}
```

## Behavior

`airc hygiene report` shows:

- free disk under the workspace filesystem
- CPU 1-minute load average
- available memory when the host OS exposes it
- GPU status placeholder for project hooks
- configured report paths
- safe cleanup candidates

`airc hygiene clean --yes` removes only rebuildable lane caches by default:

- `~/.airc-worktrees/*/src/workers/target`
- `~/.airc-worktrees/*/src/node_modules`

Main checkout caches and Docker prune are off by default. Projects can opt in
when they know the tradeoff is acceptable.

## Future Hooks

The `hooks` array is reserved for automatic project-specific checks and cleanup
actions. The intended flow is:

- lane create/remove calls the policy engine
- queue metronome warns or cleans when thresholds are crossed
- doctor includes policy status
- low-resource monitors trigger safe cleanup without waiting for a human
- project hooks add GPU, Docker, simulator, model cache, or persona sandbox
  reporting without changing the core command shape
