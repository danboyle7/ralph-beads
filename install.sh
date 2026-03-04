#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="/usr/local/bin"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Installing ralph to $INSTALL_DIR..."

ln -sf "$SCRIPT_DIR/ralph.sh" "$INSTALL_DIR/ralph"

echo "Done. You can now run 'ralph' from any project directory."
