# Developing yeschef

Guidance for working on the yeschef **source code** itself — building, testing,
architecture. If you were launched to *orchestrate* agents, that's a different role; see
`AGENTS.md`. This file is for when your job is to change yeschef's own code.

## What yeschef does

yeschef is a CLI that orchestrates multiple coding agents in parallel across git
worktrees, using tmux. One head chef agent dispatches a brigade of agents — each on its
own branch, in its own git worktree, inside its own tmux window — then supervises and
steers them. It is agent-agnostic: a line cook is just a command string launched in a
window. Requires only `git` and `tmux` (no containers, no Nix, no macOS requirement).

The orchestration "brain" is `AGENTS.md` (shipped in the repo root and written to
`~/.yeschef/` by `init`); the head chef agent reads it and drives the loop via the CLI.

Workflow:

```
yeschef init
yeschef project add <git-url> [name]
yeschef spawn <project> <branch> -p "<prompt>"   # worktree + tmux window + agent
yeschef send  <project> <branch> "<one-line steer>"
yeschef peek  <project> <branch>
yeschef status
yeschef attach [<project> <branch>]
yeschef kill  <project> <branch> [--rm-worktree]
```

## Commands

```bash
# Build
nix build          # or: cargo build

# Lint (clippy -D warnings -D clippy::pedantic)
nix build .#clippy

# Type-check without codegen
nix build .#check

# Unit tests (fast, no external deps; includes mock-backed orchestration tests)
nix build .#test   # or: cargo test --bin yeschef

# E2E tests — require git + tmux on PATH (no containers/macOS). Drive a real
# tmux session named `yeschef`, so run single-threaded.
cargo test --test e2e -- --ignored --test-threads=1
# or via the flake (PATH-checks git + tmux first):
nix run .#e2e
nix run .#e2e -- <test_name>

# Single unit test
cargo test <test_name>
```

The e2e suite is light now (no image builds). It uses unique per-test project names but
shares one global `yeschef` tmux session — `--test-threads=1` avoids cross-test races, and
each test cleans up its own window on drop.

## Verifying changes

Type-checking is not verification. Before declaring a change done, run the tests that
actually exercise it:

- Touching `store`/`names`/orchestration logic reachable from mocks → `cargo test --bin yeschef`.
- Touching the real tmux/git backends or command wiring from `main.rs` → run the relevant
  e2e test (`cargo test --test e2e -- --ignored --test-threads=1 <name>`). The e2e tests
  are the only thing that exercises real `tmux`/`git` behavior.
- Touching a single e2e test → run that specific test, not the whole suite.

## Architecture

External I/O is behind two traits in `src/backend/mod.rs`:
- `GitBackend` — wraps `git` (bare clone, worktree add/remove, config, default branch)
- `ZmxBackend` — wraps `tmux` (session/window lifecycle, send-keys, capture-pane, list)

`src/backend/real.rs` has the real implementations; `src/backend/mock.rs` has recording
mocks (the `ZmxBackend` mock tracks an in-memory window list, so orchestration logic is
unit-testable with no tmux). `Config` in `src/config.rs` holds both backends as
`Box<dyn Trait>` plus the `Store`, and is constructed in `main.rs` before dispatch.

Command logic lives in `src/commands/`:
- `init.rs` — creates `~/.yeschef/` layout, writes `AGENTS.md`, validates `git` + `tmux`.
- `project.rs` — `add` (bare clone + worktrees dir) and `list`.
- `orchestrate.rs` — `spawn`, `send`, `peek`, `status`, `kill`, `attach`. `spawn` is the
  meaty one: creates the worktree (guarded by `RollbackGuard`), ensures the tmux session,
  opens a window running the agent at the worktree, and registers the ticket in SQLite.
  The `-p` prompt is **never inlined** on the launch command line — a long prompt would
  overflow the OS arg-length limit and the agent harness, treating the giant positional
  arg as a path, dies with `ENAMETOOLONG`. Instead `spawn` writes the prompt to
  `~/.yeschef/prompts/<project>-<sanitized-branch>.md` (a stable path outside the worktree,
  so it can't be committed; overwritten on re-spawn) and launches the agent with a short
  `Read the ticket brief at <abs-path> and carry it out start to finish.` instruction. This
  is always-file (no size threshold — simpler, and correct for every prompt length) and
  agent-agnostic, since every agent takes an initial instruction as its positional arg.

State is persisted in SQLite (`~/.yeschef/yeschef.db`, via `src/store.rs`). Two tables:
`projects` (name, git_url) and `branches` — the ticket registry — (project, branch,
sanitized, window, agent).

`src/guard.rs` is a LIFO rollback guard used in `run_spawn` to undo a partial worktree if
a later step fails.

`src/names.rs` holds naming conventions: the single tmux session is `yeschef`
(`yeschef_session()`); each ticket's window is `<project>-<sanitized-branch>`
(`window_name`). Branch sanitization strips `.`/`:` so window names are safe in tmux `-t`
target args.

## tmux quirks worth knowing

- New windows are created with target `"<session>:"` (trailing colon) so tmux picks the
  next free index. A bare `-t <session>` target tries to reuse the base index and fails
  with `index N in use` under `base-index`/`renumber-windows` configs.
- `send_keys` sends the literal text then a separate `Enter` key event.
- `list-windows` on a missing session is treated as an empty list, not an error.

## Home directory

Defaults to `~/.yeschef`; overridden with `YESCHEF_HOME` (used by e2e tests for isolation).
Layout:

```
~/.yeschef/
  yeschef.db
  AGENTS.md         # kitchen manual, refreshed by `init`
  projects/
    <project>/
      .bare/        # bare git clone
      worktrees/
        <branch>/   # git worktree (one per ticket)
```
