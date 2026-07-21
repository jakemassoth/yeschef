# yeschef

Orchestrate multiple coding agents in parallel across git worktrees, using
[herdr](https://github.com/ogulcancelik/herdr) (an "agent multiplexer") as the terminal
layer.

yeschef lets **one** agent session (the head chef) dispatch and supervise a brigade of
agents, each working on its own branch in its own git worktree inside its own workspace of a
shared `yeschef` herdr session. You talk to the head chef; it spawns line cooks, steers
them, reads their output, and reports back. It is **agent-agnostic** — a line cook is just a
command launched in a herdr pane (`claude`, `codex`, `aider`, …), so nothing is tied to a
particular vendor. `yeschef attach` hands the human herdr's native UI, where the whole
brigade shows as live, status-coloured workspaces (herdr detects each agent's state).

> Inspired by [firstmate](https://github.com/kunchenguid/firstmate). Where firstmate is
> bash scripts + an `AGENTS.md`, yeschef is a single Rust CLI that *is* the toolbelt, plus
> an `AGENTS.md` that teaches the head chef to use it.

## Requirements

`git` on your `PATH`, plus either Nix or Rust/Cargo to run yeschef itself from source (see
[Running yeschef](#running-yeschef)). `nix run` bakes a bundled `herdr` onto yeschef's
PATH, so you don't need one pre-installed; a Cargo build falls back to a `herdr` on your
`PATH`. No containers, no macOS requirement.

## Workflow

```bash
yeschef init                                  # ~/yeschef + AGENTS.md
yeschef project add <git-url> [name]          # bare clone + worktrees dir
yeschef refresh [<project>]                   # git fetch --prune (all projects if omitted)

# dispatch a line cook: worktree + herdr workspace + agent
yeschef spawn <project> <branch> -p "Implement X and summarize what changed"

yeschef status                                # who's running / gone + herdr's live state
yeschef peek  <project> <branch>              # read an agent's pane
yeschef send  <project> <branch> "use the helper in utils.rs"   # one-line steer
yeschef tui                                   # attach to herdr's native brigade UI
yeschef attach [<project> <branch>]           # watch the brigade live
yeschef restart                               # bounce the herdr server; workspaces restored, agents resumed
yeschef kill  <project> <branch> --rm-worktree
yeschef cleanup [<project>] [--yes]           # reap merged/gone + DONE tickets (dry run unless --yes)
```

`spawn --agent <cmd>` chooses the harness (default `claude`); `-p/--prompt` is passed as
the agent's first argument.

## Running yeschef

yeschef is never installed or on your `PATH`. The bare `yeschef <args>` written above (and
throughout `AGENTS.md`) is **shorthand** — you always run it from the canonical source
checkout at **`~/yeschef/yeschef-src`**, which works from any directory and always runs
the latest source there:

```bash
nix run ~/yeschef/yeschef-src -- <args>                          # reproducible default
cargo run --manifest-path ~/yeschef/yeschef-src/Cargo.toml -- <args>   # faster for tight loops
```

`AGENTS.md` is the head chef's manual — the dispatch → supervise → land → teardown loop.
`yeschef init` writes a copy to `~/yeschef/`, and it also ships in the source checkout.

## Development

```bash
nix build              # or: cargo build
nix build .#clippy     # clippy -D warnings -D clippy::pedantic
nix build .#test       # or: cargo test  (unit tests, no external deps)

# e2e (real git + herdr; no containers/macOS needed)
cargo test --test e2e -- --ignored
nix run .#e2e          # puts git + herdr on PATH first
```

## Status

The core orchestration loop (spawn / send / peek / status / kill / attach / restart). Heavier
firstmate-style features — an event-driven supervision watcher, ship/scout ticket types,
PR/merge automation, and persistent specialist agents — are not implemented yet.
