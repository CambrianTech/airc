#!/bin/bash
# Install claude-relay — adds `relay` command to PATH
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${HOME}/.local/bin"

mkdir -p "$INSTALL_DIR"

# Symlink relay to PATH
ln -sf "$SCRIPT_DIR/relay" "$INSTALL_DIR/relay"

# Ensure ~/.local/bin is in PATH
if ! echo "$PATH" | grep -q "$INSTALL_DIR"; then
  echo ""
  echo "Add to your shell profile (~/.zshrc or ~/.bashrc):"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi

echo "Installed: relay → $INSTALL_DIR/relay"
echo ""
echo "Quick start:"
echo "  relay init <your-name>"
echo "  relay pair <peer-name> <user@host>"
echo "  relay send <peer> 'hello'"
