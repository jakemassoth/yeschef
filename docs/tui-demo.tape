# Demo: `yeschef tui` is now the native tmux UI. `yeschef tui` just attaches to
# the single `yeschef` session, so tmux's own status line IS the brigade view —
# one colour-coded tab per cook (CHEF ★ magenta · IN_PROGRESS ● yellow ·
# DONE ✓ green · BLOCKED ■ red · NEW ○ grey), switched with prefix+n/p/<n>,
# head chef always one keystroke away at prefix+0, clean prefix+d detach.
#
# Render headlessly (inside `nix develop`), after `cargo build`:
#   nix develop --command vhs docs/tui-demo.tape   → docs/tui-demo.gif

Output docs/tui-demo.gif

Set FontSize 13
Set Width 1100
Set Height 380
Set Padding 8

# Off-camera: stand up a throwaway kitchen with a few fake cooks (see
# docs/tui-demo-setup.sh). Sourced so `yeschef tui` inherits the throwaway
# socket and never touches the live server.
Hide
Type "source docs/tui-demo-setup.sh"
Enter
Sleep 5s
Type "clear"
Enter
Show

# Attach to the brigade: the tab bar shows every cook, colour-coded by status.
Type "./target/debug/yeschef tui"
Enter
Sleep 3s

# Switch between cooks with tmux's own next-window (prefix+n).
Ctrl+b
Type "n"
Sleep 1500ms
Ctrl+b
Type "n"
Sleep 1500ms

# Back to the head chef — always window 0, one keystroke away (prefix+0).
Ctrl+b
Type "0"
Sleep 2s

# A cook reports BLOCKED (scheduled by the setup): its tab recolours live,
# grey ○ → red ■, no polling.
Sleep 4s

# Detach cleanly back to the shell — tmux's built-in prefix+d.
Ctrl+b
Type "d"
Sleep 1500ms

# Off-camera: tear down the throwaway tmux server.
Hide
Type "tmux -L $YESCHEF_TMUX_SOCKET kill-server 2>/dev/null; true"
Enter
Sleep 1s
