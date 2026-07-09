# Demo for the zmx -> tmux backend switch: the TUI previews a line cook's live
# (coloured) tmux session, Enter attaches to it for full interaction, and
# `Ctrl+b d` — tmux's own detach — returns cleanly to the TUI. That clean
# detach is the whole point of the switch (zmx's `Ctrl+\` detach was broken).
#
# Setup is driven headlessly by docs/tmux-backend-demo-setup.sh, which spawns a
# long-lived colourful line cook under a throwaway YESCHEF_HOME.
#
# Render: nix develop --command bash docs/tmux-backend-demo-setup.sh \
#           && nix develop --command vhs docs/tmux-backend-demo.tape

Output docs/tmux-backend-demo.gif

Set FontSize 14
Set Width 1200
Set Height 680
Set Padding 12

Hide
Type "export YESCHEF_HOME=/tmp/yeschef-tmux-demo YESCHEF_SRC=/tmp/yeschef-tmux-demo-nosrc"
Enter
Type "clear"
Enter
Show

# List mode: the right pane previews the selected line cook's live tmux session,
# colours and all, via a real VT100 parser.
Type "./target/debug/yeschef tui"
Enter
Sleep 2s

# Move off the pinned head chef onto the line cook.
Type "j"
Sleep 3s

# Enter hands the real terminal to `tmux attach` for direct interaction.
Enter
Sleep 3s

# Detach with tmux's own binding — Ctrl+b then d — and land back in the TUI.
Ctrl+b
Sleep 500ms
Type "d"
Sleep 2500ms

# Back in the TUI: navigate to prove it's live again after the detach.
Type "k"
Sleep 1500ms
Type "j"
Sleep 2s

# Quit the TUI.
Type "q"
Sleep 1s
