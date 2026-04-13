#!/usr/bin/env bash
#
# Agent Relay installer
#
# curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
#
# Clones the repo, puts `relay` on PATH, symlinks skills into ~/.claude/skills/

set -euo pipefail

REPO_URL="https://github.com/CambrianTech/agent-relay.git"
CLONE_DIR="${AGENT_RELAY_DIR:-$HOME/.agent-relay-src}"
BIN_DIR="$HOME/.local/bin"
SKILLS_TARGET="$HOME/.claude/skills"

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }

# ── Clone or update ─────────────────────────────────────────────────────

if [ -d "$CLONE_DIR/.git" ]; then
  info "Updating existing install"
  git -C "$CLONE_DIR" pull --ff-only --quiet
else
  info "Installing agent-relay"
  git clone --quiet "$REPO_URL" "$CLONE_DIR"
fi

# ── relay on PATH ───────────────────────────────────────────────────────

mkdir -p "$BIN_DIR"
ln -sf "$CLONE_DIR/relay" "$BIN_DIR/relay"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
  # Add to shell profile automatically
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    if [ -f "$rc" ] && ! grep -q 'agent-relay' "$rc"; then
      echo 'export PATH="$HOME/.local/bin:$PATH"  # agent-relay' >> "$rc"
      ok "Added ~/.local/bin to PATH in $(basename "$rc")"
      break
    fi
  done
  export PATH="$BIN_DIR:$PATH"
fi

# ── Skills into Claude Code ─────────────────────────────────────────────

if [ -d "$CLONE_DIR/skills" ]; then
  mkdir -p "$SKILLS_TARGET"

  # Clean up old relay-prefixed symlinks from previous installs
  for old in "$SKILLS_TARGET"/relay-*; do
    [ -L "$old" ] && rm "$old" && info "Removed old symlink: $(basename "$old")"
  done

  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$SKILLS_TARGET/$skill_name"
    [ -L "$target" ] && rm "$target"
    ln -sf "$skill_dir" "$target"
    ok "Skill: /relay:$skill_name"
  done
fi

# ── SSH key for relay connections ───────────────────────────────────────

RELAY_HOME="$HOME/.agent-relay"
IDENTITY_DIR="$RELAY_HOME/identity"
mkdir -p "$RELAY_HOME" "$IDENTITY_DIR" "$RELAY_HOME/peers"

ssh_key="$IDENTITY_DIR/ssh_key"
if [ ! -f "$ssh_key" ]; then
  ssh-keygen -t ed25519 -f "$ssh_key" -N "" -C "agent-relay" -q
  ok "Generated SSH key"
fi

mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"
pubkey=$(cat "${ssh_key}.pub")
if ! grep -qF "$pubkey" "$HOME/.ssh/authorized_keys" 2>/dev/null; then
  echo "$pubkey" >> "$HOME/.ssh/authorized_keys"
  chmod 600 "$HOME/.ssh/authorized_keys"
  ok "Added relay key to authorized_keys"
fi

# ── Ensure SSH daemon is running ────────────────────────────────────────

if ! nc -z localhost 22 2>/dev/null || ! ssh -i "$ssh_key" -o IdentitiesOnly=yes -o ConnectTimeout=3 -o StrictHostKeyChecking=accept-new localhost "echo ok" >/dev/null 2>&1; then
  info "Enabling SSH (Remote Login)..."
  if [ "$(uname)" = "Darwin" ]; then
    # Try to get sshd working — check why launchd won't spawn it
    info "Checking sshd config..."
    sudo /usr/sbin/sshd -t 2>&1 && ok "sshd config OK" || info "sshd config error (see above)"
    info "Checking system log for sshd..."
    log show --predicate 'process == "sshd"' --last 2m --style compact 2>/dev/null | tail -5 || true
    # Try triggering sshd spawn by connecting
    ssh -i "$ssh_key" -o IdentitiesOnly=yes -o ConnectTimeout=3 -o StrictHostKeyChecking=accept-new localhost "echo ok" >/dev/null 2>&1 && { ok "SSH is working"; } || {
      info "sshd not spawning. Trying direct start on port 2222 as fallback..."
      sudo /usr/sbin/sshd -p 2222 2>&1 || true
      if ssh -i "$ssh_key" -o IdentitiesOnly=yes -o ConnectTimeout=3 -o StrictHostKeyChecking=accept-new -p 2222 localhost "echo ok" >/dev/null 2>&1; then
        ok "SSH working on port 2222 (sshd started manually)"
        # Save port to config so relay uses it
        if [ -f "$RELAY_HOME/config.json" ]; then
          python3 -c "
import json
c = json.load(open('$RELAY_HOME/config.json'))
c['ssh_port'] = 2222
json.dump(c, open('$RELAY_HOME/config.json','w'), indent=2)
" 2>/dev/null
        fi
      else
        info "SSH not working. System log:"
        log show --predicate 'process == "sshd"' --last 1m --style compact 2>/dev/null | tail -10 || true
      fi
    }
  else
    sudo -n systemctl start sshd 2>/dev/null \
      || sudo -n service ssh start 2>/dev/null \
      || sudo -n /usr/sbin/sshd 2>/dev/null \
      || true
    sleep 1
    if ssh -i "$ssh_key" -o IdentitiesOnly=yes -o ConnectTimeout=3 -o StrictHostKeyChecking=accept-new localhost "echo ok" >/dev/null 2>&1; then
      ok "SSH is working"
    else
      info "Could not start sshd. Run: sudo systemctl start sshd"
    fi
  fi
else
  ok "SSH is working"
fi

# ── Done ────────────────────────────────────────────────────────────────

echo ""
ok "Installed."
echo ""
echo "  relay start <your-name>    # on this machine"
echo "  relay join <name@host>     # on the other machine"
echo ""
