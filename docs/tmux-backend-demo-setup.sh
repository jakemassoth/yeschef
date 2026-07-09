#!/usr/bin/env bash
# Headless setup for docs/tmux-backend-demo.tape: a throwaway YESCHEF_HOME with
# one long-lived, colourful line cook so the TUI preview has something to show.
# Run from the repo root before rendering the tape.
set -euo pipefail

export YESCHEF_HOME=/tmp/yeschef-tmux-demo
# Drive a throwaway tmux `-L` socket, NOT the operator's live `yeschef` server:
# the reset `kill-server` below would otherwise nuke every running line cook.
export YESCHEF_TMUX_SOCKET=yeschef-demo
REPO=/tmp/yeschef-tmux-demo-repo
COOK=/tmp/yeschef-demo-cook.sh
BIN=./target/debug/yeschef

cargo build --bin yeschef

# A colourful, long-lived line cook so the TUI preview stays lively.
cat >"$COOK" <<'COOKEOF'
i=0
while true; do
  printf "\033[36m[line-cook]\033[0m building widget subsystem  step \033[33m%s\033[0m\n" "$i"
  printf "  \033[32m✓ compiled\033[0m module-%s.rs   \033[35m(cache hit)\033[0m\n" "$i"
  i=$((i + 1))
  sleep 1
done
COOKEOF

rm -rf "$YESCHEF_HOME" "$REPO"
tmux -L "$YESCHEF_TMUX_SOCKET" kill-server 2>/dev/null || true

mkdir -p "$REPO"
git -C "$REPO" init -q -b main
git -C "$REPO" config user.email demo@yeschef.test
git -C "$REPO" config user.name demo
echo "# demo" >"$REPO/README.md"
git -C "$REPO" add .
git -C "$REPO" commit -qm init

"$BIN" init >/dev/null
"$BIN" project add "file://$REPO" demo >/dev/null
"$BIN" spawn demo build-widget --agent "sh $COOK" >/dev/null
echo "demo line cook spawned; now render: vhs docs/tmux-backend-demo.tape"
