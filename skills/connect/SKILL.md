---
name: relay:connect
description: Connect to Agent Relay — host or join another machine.
user-invocable: true
allowed-tools: Bash, Monitor
argument-hint: "<name@user@host#key>"
---

# Connect to Agent Relay

Do everything yourself — don't ask the user to run commands.

## 1. Install if needed

If `relay` is not on PATH:
```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
```

## 2. Connect

Run `relay connect` with the full arguments. It handles everything — init, pair, and starts monitoring automatically. It blocks (streaming messages), so use it with the Monitor tool:

**If `$ARGUMENTS` contains `@`** — joining a host:
```
Monitor(persistent=true, command="relay connect $ARGUMENTS")
```
Then send a test:
```bash
relay send <peer-name> "connected"
```

**If no arguments** — you are the host:
```
Monitor(persistent=true, command="relay connect")
```
Then show the join string from the output to the user. Tell them: "Give this to the other Claude:" followed by `/relay:connect <the join string>`

## 3. If connect fails

It prints the actual error. Read it.

- **Host mode, SSH not working:** it prints the exact sudo command needed. Tell the user: "Please run this:" and show them the command. In Claude Code they can type `! sudo ...` to run it. Then retry.
- **Join mode, can't reach host:** check the host is running `relay connect` and the address is correct.
