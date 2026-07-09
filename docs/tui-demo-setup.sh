# Off-camera setup for the native tmux TUI demo (docs/tui-demo.tape).
#
# SOURCE this from the recording shell (`source docs/tui-demo-setup.sh`) — it
# exports YESCHEF_* into the caller so the `yeschef tui` that follows drives a
# THROWAWAY tmux socket, never the operator's live `yeschef` server.
#
# It stands up a throwaway yeschef home with a pinned head chef and three fake
# line cooks in different statuses, then schedules one live status flip a few
# seconds out so the recording captures a tab recolouring in real time.

export YESCHEF_HOME="${TMPDIR:-/tmp}/yeschef-tui-demo-home"
export YESCHEF_TMUX_SOCKET="yeschef-demo-$$"
export YESCHEF_SRC="$YESCHEF_HOME"
export YESCHEF_HEADCHEF_CMD="sh -c 'exec sleep 600'"

(
  set -e
  BIN=./target/debug/yeschef
  rm -rf "$YESCHEF_HOME"

  SAMPLE="${TMPDIR:-/tmp}/yeschef-tui-demo-sample"
  rm -rf "$SAMPLE"
  mkdir -p "$SAMPLE"
  git -C "$SAMPLE" init -q -b main
  git -C "$SAMPLE" config user.email demo@demo.test
  git -C "$SAMPLE" config user.name Demo
  echo "# demo" >"$SAMPLE/README.md"
  git -C "$SAMPLE" add .
  git -C "$SAMPLE" commit -qm initial

  "$BIN" init >/dev/null
  "$BIN" project add "file://$SAMPLE" proj >/dev/null 2>&1
  for b in fix-auth new-api db-migrate; do
    "$BIN" spawn proj "$b" --base origin/main \
      --agent "sh -c 'exec sleep 600'" >/dev/null 2>&1
  done

  # Two cooks start with a status (yellow ● / green ✓); db-migrate stays NEW
  # (grey ○) so the scheduled flip below is a visible live change.
  "$BIN" ticket proj fix-auth status-set IN_PROGRESS >/dev/null
  "$BIN" ticket proj new-api status-set DONE >/dev/null

  # Live flip: a few seconds into the attach, db-migrate reports BLOCKED and its
  # tab recolours grey ○ → red ■ with no polling and no redraw code of ours.
  (sleep 14; "$BIN" ticket proj db-migrate status-set BLOCKED >/dev/null 2>&1) &
)
