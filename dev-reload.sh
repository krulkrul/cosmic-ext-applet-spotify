#!/usr/bin/env bash
set -e
cargo build --release
pkill -f "cosmic-ext-applet-spotify" 2>/dev/null || true
echo "Reloaded. Panel will restart the applet automatically."
