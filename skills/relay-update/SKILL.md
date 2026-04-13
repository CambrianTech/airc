---
name: relay:update
description: Update Agent Relay to the latest version from GitHub.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# Update Agent Relay

Pull the latest version and re-link skills.

```bash
cd ~/.agent-relay-src && git pull --ff-only && ./install.sh
```

Report what changed (new skills, bug fixes) by checking `git log --oneline -5`.
