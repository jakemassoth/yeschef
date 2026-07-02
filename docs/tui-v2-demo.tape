# Proof-of-behaviour recording for the TUI v2 rework: colours render, long
# lines wrap/clip cleanly at the pane edge instead of corrupting, j/k
# navigates the brigade, and Enter hands off to a real `zmx attach` for
# full-fidelity interaction (Ctrl+\ returns to the list).
#
# Prerequisites — a yeschef home with at least one running line cook whose
# session prints ANSI colour and a line longer than the pane, e.g.:
#   export YESCHEF_HOME=/tmp/yeschef-tui-demo-home ZMX_DIR=/tmp/yeschef-tui-demo-zmx
#   cargo build
#   ./target/debug/yeschef init
#   ./target/debug/yeschef project add <any-git-repo> demo
#   ./target/debug/yeschef spawn demo colors -p "..."   # then point its zmx
#   ./target/debug/yeschef spawn demo status -p "..."   # session at a script
#                                                          that prints colour
#
# Render headlessly: nix develop --command vhs docs/tui-v2-demo.tape

Output tui-v2-demo.gif

Set FontSize 13
Set Width 1200
Set Height 650
Set Padding 10

Hide
Type "export YESCHEF_HOME=/tmp/yeschef-tui-demo-home ZMX_DIR=/tmp/yeschef-tui-demo-zmx"
Enter
Type "clear"
Enter
Show

# List mode: colourised preview of the selected line cook.
Type "./target/debug/yeschef tui"
Enter
Sleep 3s

# Navigate the brigade with vim keys.
Type "j"
Sleep 2s
Type "k"
Sleep 1s

# Focus mode: Enter hands the real terminal to `zmx attach` for direct
# interaction — full native colour/wrap/keyboard fidelity.
Enter
Sleep 3s

# Leave focus mode back to the list — zmx's own detach binding.
Ctrl+\
Sleep 2s

# Quit the TUI with its own quit key.
Type "q"
Sleep 1s
