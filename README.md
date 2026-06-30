# yeschef

Orchestrate multiple coding agents in parallel across git worktrees, using tmux.

yeschef lets **one** agent session (the head chef) dispatch and supervise a brigade of
agents, each working on its own branch in its own git worktree inside its own tmux
window. You talk to the head chef; it spawns line cooks, steers them, reads their
output, and reports back. It is **agent-agnostic** — a line cook is just a command launched
in a window (`claude`, `codex`, `aider`, …), so nothing is tied to a particular vendor.

> Inspired by [firstmate](https://github.com/kunchenguid/firstmate). Where firstmate is
> bash scripts + an `AGENTS.md`, yeschef is a single Rust CLI that *is* the toolbelt, plus
> an `AGENTS.md` that teaches the head chef to use it.

## Requirements

`git` and `tmux` on your `PATH`. That's it — no containers, no Nix, no macOS requirement.

## Workflow

```bash
yeschef init                                  # ~/.yeschef + AGENTS.md
yeschef project add <git-url> [name]          # bare clone + worktrees dir
yeschef refresh [<project>]                   # git fetch --prune (all projects if omitted)

# dispatch a line cook: worktree + tmux window + agent
yeschef spawn <project> <branch> -p "Implement X and summarize what changed"

yeschef status                                # who's running / dead / gone
yeschef peek  <project> <branch>              # read an agent's pane
yeschef send  <project> <branch> "use the helper in utils.rs"   # one-line steer
yeschef attach [<project> <branch>]           # watch the brigade live
yeschef kill  <project> <branch> --rm-worktree
```

`spawn --agent <cmd>` chooses the harness (default `claude`); `-p/--prompt` is passed as
the agent's first argument.

Run your head chef agent from `~/.yeschef` so it loads `AGENTS.md` — the manual that
describes the dispatch → supervise → land → teardown loop.

## Development

```bash
nix build              # or: cargo build
nix build .#clippy     # clippy -D warnings -D clippy::pedantic
nix build .#test       # or: cargo test  (unit tests, no external deps)

# e2e (real git + tmux; no containers/macOS needed)
cargo test --test e2e -- --ignored --test-threads=1
nix run .#e2e          # PATH-checks git + tmux first
```

## Status

The core orchestration loop (spawn / send / peek / status / kill / attach). Heavier
firstmate-style features — an event-driven supervision watcher, ship/scout ticket types,
PR/merge automation, and persistent specialist agents — are not implemented yet.
