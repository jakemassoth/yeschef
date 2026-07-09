# VHS tape for the yeschef TUI-replacement spike PoC.
# Regenerate:  nix develop --command vhs docs/spikes/poc/tui-native.tape
# (the dev shell provides vhs + tmux). Produces tui-native-demo.gif.
#
# Shows the whole TUI spec running on stock tmux, no custom rendering:
#   - the brigade tab-bar, colour-coded by each cook's status  (req 3)
#   - switching between cooks with prefix+n                     (req 1)
#   - jumping back to the head chef with prefix+0               (req 2)
#   - live status->UI update via `status-set`                  (req 3)
#   - a clean detach back to the shell                          (the zmx bug fixed)

Output docs/spikes/poc/tui-native-demo.gif

Set FontSize 15
Set Width 1320
Set Height 560
Set Padding 12
Set TypingSpeed 60ms

# --- off-camera: quiet prompt + stand up the demo brigade ---
Hide
Type "export PS1='$ '; clear" Enter
Type "cd $(git rev-parse --show-toplevel)" Enter
Type "docs/spikes/poc/tui-native.sh teardown >/dev/null 2>&1; docs/spikes/poc/tui-native.sh setup >/dev/null" Enter
Type "clear" Enter
Show

# --- attach: this is what `yeschef tui` becomes ---
Type "docs/spikes/poc/tui-native.sh attach"
Sleep 800ms
Enter
Sleep 2.5s

# tab bar shows head chef + 4 cooks, each coloured by status (req 3).
# switch between cooks with tmux's own key: prefix+n (req 1).
Ctrl+b
Type "n"
Sleep 1.5s
Ctrl+b
Type "n"
Sleep 1.5s
Ctrl+b
Type "n"
Sleep 1.5s

# jump straight back to the head chef — window 0 (req 2).
Ctrl+b
Type "0"
Sleep 2s

# detach cleanly back to the shell (prefix+d). The whole reason we left zmx.
Ctrl+b
Type "d"
Sleep 1.5s

# --- live status -> UI: flip a cook's status from the CLI, re-attach ---
Type "docs/spikes/poc/tui-native.sh status-set proj-flaky-test IN_PROGRESS"
Enter
Sleep 1.5s
Type "docs/spikes/poc/tui-native.sh attach"
Enter
Sleep 2.5s

# the proj-flaky-test tab has gone yellow (● IN_PROGRESS), live.
Ctrl+b
Type "d"
Sleep 1s

Hide
Type "docs/spikes/poc/tui-native.sh teardown >/dev/null 2>&1" Enter
