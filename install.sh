#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Installing ralph with cargo..."
cargo install --path "$SCRIPT_DIR" --bin ralph --force
echo "Done. You can now run 'ralph' from any project directory."
