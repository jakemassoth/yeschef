#!/usr/bin/env bash
# yeschef TUI-replacement spike — runnable proof-of-concept.
#
# Demonstrates the recommendation in ../tui-replacement.md: replace the custom
# ratatui/vt100 TUI with tmux's own native UI. One `yeschef` tmux session holds
# the head chef as window 0 and every line cook as its own window; tmux's status
# line becomes the brigade tab-bar, colour-coded live by each cook's status.
#
# It proves the whole TUI spec against real tmux, with fake cooks so it runs
# anywhere (no projects/agents needed):
#
#   1. See what each cook is doing  -> attach; each cook is a tmux window/tab.
#   2. Get back to the head chef    -> head chef is window 0 (prefix+0 / prefix+c).
#   3. At-a-glance status per cook  -> the tab-bar glyph+colour, driven by @status.
#
# Every tmux command here is exactly what the yeschef Rust backend would run
# (the `# BACKEND:` comments map each to its would-be TmuxBackend method).
#
# Usage:
#   ./tui-native.sh setup                 # stand up the demo brigade
#   ./tui-native.sh status-set <win> <S>  # e.g. status-set proj-fix-auth DONE
#   ./tui-native.sh attach                # open the native TUI (detach: prefix+d)
#   ./tui-native.sh teardown
#
# Everything runs on a private `-L` socket + `-f` config, exactly like yeschef,
# so it never touches your own tmux server or ~/.tmux.conf.
set -euo pipefail

SOCKET="yeschef-poc"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONF="$HERE/tui-native.tmux.conf"
SESSION="yeschef"

# tmux, pinned to yeschef's private server (socket + our config). This is the
# `RealTmuxBackend::cmd()` helper.
t() { tmux -L "$SOCKET" -f "$CONF" "$@"; }

# A fake long-lived "agent" that prints a banner then idles, so an attached tmux
# window shows something. Real yeschef launches `claude`/`codex`/… here instead.
fake_agent() { printf 'sh -lc %q' "printf '\033[1;36m[%s]\033[0m line cook ready — this is a live tmux window\n' '$1'; exec sleep 100000"; }

cmd_setup() {
  t kill-session -t "$SESSION" 2>/dev/null || true

  # Head chef = window 0 of the single brigade session.
  # BACKEND: new_window(session=yeschef, window=headchef, cwd=src, command=claude)
  t new-session -d -s "$SESSION" -x 200 -y 50 -n "headchef" "$(fake_agent 'HEAD CHEF')"
  t set-window-option -t "$SESSION:0" @status CHEF

  # Each line cook = a new window in the SAME session. The window name is the
  # stable ticket id (project-branch) and doubles as the send/peek/kill target.
  # BACKEND: new_window(session=yeschef, window=<project>-<branch>, cwd=worktree, command=<agent>)
  for spec in "proj-fix-auth:IN_PROGRESS" "proj-new-api:DONE" \
              "proj-db-migrate:BLOCKED" "proj-flaky-test:NEW"; do
    win="${spec%%:*}"; status="${spec##*:}"
    t new-window -d -t "$SESSION:" -n "$win" "$(fake_agent "$win")"
    t set-window-option -t "$SESSION:$win" @status "$status"
  done

  echo "brigade is up on the private '$SOCKET' server:"
  cmd_list
  echo
  echo "open the native TUI:   $0 attach     (detach with prefix+d, i.e. Ctrl+b d)"
  echo "change a cook status:  $0 status-set proj-fix-auth DONE"
}

# The status -> UI propagation. This single tmux call is the entire mechanism:
# yeschef would run it inside `run_ticket_status_set`, right after the SQLite
# write it already does. The tab-bar re-renders live from @status; no polling.
# BACKEND: set_window_status(session=yeschef, window=<win>, status=<S>)
cmd_status_set() {
  local win="$1" status="$2"
  t set-window-option -t "$SESSION:$win" @status "$status"
  echo "set @status of '$win' to $status — attached clients update immediately"
}

# The brigade, read straight from tmux (no separate registry needed for the
# view). BACKEND: list_windows(session=yeschef) + the @status option.
cmd_list() {
  t list-windows -t "$SESSION" \
    -F '  [#{window_index}] #{window_name}  status=#{@status}'
}

# BACKEND: attach(session=yeschef). `yeschef tui` becomes ~this one line.
cmd_attach() { t attach -t "$SESSION"; }

cmd_teardown() { t kill-server 2>/dev/null || true; echo "torn down."; }

case "${1:-}" in
  setup)       cmd_setup ;;
  status-set)  cmd_status_set "${2:?window}" "${3:?status}" ;;
  list)        cmd_list ;;
  attach)      cmd_attach ;;
  teardown)    cmd_teardown ;;
  *) echo "usage: $0 {setup|status-set <win> <STATUS>|list|attach|teardown}" >&2; exit 2 ;;
esac
