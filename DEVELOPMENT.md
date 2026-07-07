# Developing yeschef

Guidance for working on the yeschef **source code** itself — building, testing,
architecture. If you were launched to *orchestrate* agents, that's a different role; see
`AGENTS.md`. This file is for when your job is to change yeschef's own code.

## What yeschef does

yeschef is a CLI that orchestrates multiple coding agents in parallel across git
worktrees, using zmx. One head chef agent dispatches a brigade of agents — each on its
own branch, in its own git worktree, inside its own zmx session — then supervises and
steers them. It is agent-agnostic: a line cook is just a command string launched in a
zmx session. Requires only `git` and `zmx` (no containers, no Nix, no macOS requirement).

The orchestration "brain" is `AGENTS.md` (shipped in the repo root and written to
`~/yeschef/` by `init`); the head chef agent reads it and drives the loop via the CLI.

Workflow:

```
yeschef init
yeschef project add <git-url> [name]
yeschef spawn <project> <branch> -p "<prompt>"   # worktree + zmx session + agent
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

# E2E tests — require git + zmx on PATH (no containers/macOS). Drive real zmx
# sessions sharing the `yeschef` brigade name, so run single-threaded.
cargo test --test e2e -- --ignored --test-threads=1
# or via the flake (PATH-checks git + zmx first):
nix run .#e2e
nix run .#e2e -- <test_name>

# Single unit test
cargo test <test_name>
```

The e2e suite is light now (no image builds). It uses unique per-test project names but
shares the global `yeschef` zmx session namespace — `--test-threads=1` avoids cross-test
races, and each test cleans up its own zmx session on drop.

## CI — run `nix flake check` before you push

CI is driven entirely by Nix flake checks. Run the whole sandboxed suite locally
**before pushing** so CI doesn't fail after the fact:

```bash
nix flake check        # fmt (rustfmt) + nixfmt + lint (clippy) + unit tests
nix run .#e2e          # the e2e suite (run separately — see below)
```

`checks` in `flake.nix` covers **fmt** (`cargo fmt --check`), **nixfmt**
(nixfmt-rfc-style on `flake.nix`), **lint** (strict clippy), and **test** (unit
tests). The **e2e** suite is deliberately *not* a flake check: it drives a real
zmx session and real git worktrees (impure, shares the global `yeschef` zmx
namespace), so it runs un-sandboxed via `nix run .#e2e`. Run both before pushing.

The GitHub Action (`.github/workflows/ci.yml`) runs exactly these two commands on
every push and PR, on a `macos-14` runner (zmx is Apple-SDK-coupled and the suite
is verified on macOS). It installs Nix with the
[Determinate Nix action](https://github.com/determinatesystems/determinate-nix-action).

## Verifying changes

Type-checking is not verification. Before declaring a change done, run the tests that
actually exercise it:

- Touching `store`/`names`/orchestration logic reachable from mocks → `cargo test --bin yeschef`.
- Touching the real zmx/git backends or command wiring from `main.rs` → run the relevant
  e2e test (`cargo test --test e2e -- --ignored --test-threads=1 <name>`). The e2e tests
  are the only thing that exercises real `zmx`/`git` behavior.
- Touching a single e2e test → run that specific test, not the whole suite.

## Recording the terminal (demos / repros)

The dev shell ships `vhs` (charmbracelet/vhs) so you can record a terminal
session headlessly and attach a gif/mp4 to a PR. The `terminal-recording` skill
(`.claude/skills/terminal-recording/SKILL.md`) is the quick-reference. A worked
example lives in `docs/tui-demo.tape` (records `yeschef tui` →
`docs/tui-demo.gif`); regenerate it with `nix develop --command vhs
docs/tui-demo.tape`.

## Architecture

External I/O is behind two traits in `src/backend/mod.rs`:
- `GitBackend` — wraps `git` (bare clone, worktree add/remove, config, default branch)
- `ZmxBackend` — wraps `zmx` (session lifecycle via `run`/`send`/`history`/`ls`/`kill`/`attach`)

`src/backend/real.rs` has the real implementations; `src/backend/mock.rs` has recording
mocks (the `ZmxBackend` mock tracks an in-memory window list, so orchestration logic is
unit-testable with no zmx). `Config` in `src/config.rs` holds both backends as
`Box<dyn Trait>` plus the `Store`, and is constructed in `main.rs` before dispatch.

Command logic lives in `src/commands/`:
- `init.rs` — creates `~/yeschef/` layout, writes `AGENTS.md`, validates `git` + `zmx`.
- `project.rs` — `add` (bare clone + worktrees dir) and `list`.
- `orchestrate.rs` — `spawn`, `send`, `peek`, `status`, `kill`, `attach`. `spawn` is the
  meaty one: creates the worktree (guarded by `RollbackGuard`), ensures the brigade session,
  opens a zmx session running the agent at the worktree, and registers the ticket in SQLite.
  The `-p` prompt is **never inlined** on the launch command line — a long prompt would
  overflow the OS arg-length limit and the agent harness, treating the giant positional
  arg as a path, dies with `ENAMETOOLONG`. Instead `spawn` writes the prompt to
  `~/yeschef/prompts/<project>-<sanitized-branch>.md` (a stable path outside the worktree,
  so it can't be committed; overwritten on re-spawn) and launches the agent with a short
  `Read the ticket brief at <abs-path> and carry it out start to finish.` instruction. This
  is always-file (no size threshold — simpler, and correct for every prompt length) and
  agent-agnostic, since every agent takes an initial instruction as its positional arg.

State is persisted in SQLite (`~/yeschef/yeschef.db`, via `src/store.rs`). Two tables:
`projects` (name, git_url) and `branches` — the ticket registry — (project, branch,
sanitized, window, agent).

`src/guard.rs` is a LIFO rollback guard used in `run_spawn` to undo a partial worktree if
a later step fails.

`src/names.rs` holds naming conventions: the brigade session name is `yeschef`
(`yeschef_session()`), which namespaces every ticket's zmx session; each ticket's window is
`<project>-<sanitized-branch>` (`window_name`), embedded into the zmx session id
`yeschef-<window>`. Branch sanitization strips `.`/`:` (historically tmux `-t` target
separators) so the derived name stays a clean zmx session id.

## zmx quirks worth knowing

- zmx has no window concept: each ticket "window" is a standalone zmx session named
  `<session>-<window>`, created lazily by `zmx run -d`. There's no parent session to
  pre-create, so `ensure_session` is a no-op.
- `send_keys` writes the literal text with `zmx send`, then sends a separate carriage
  return (`\r`) event to submit it.
- `list_windows`/`session_exists` derive the brigade's state from `zmx ls --short`; a
  missing session yields an empty list, not an error. zmx exposes no per-session
  active/dead state, so a finished ticket's session simply disappears — it surfaces as
  "gone" in `status`, never "dead".
- **`Ctrl+\` detach vs. enhanced keyboard protocols.** zmx detaches when it reads the
  raw byte `0x1C` (`Ctrl+\`). A focused session running a full-screen app that enables
  an *enhanced keyboard protocol* — xterm "modifyOtherKeys" (`CSI > 4 ; 2 m`) or the
  kitty keyboard protocol — makes the terminal encode `Ctrl+\` as an escape sequence
  (`CSI 27 ; 5 ; 92 ~`) instead of `0x1C`, and `zmx attach` replays that mode to the
  client on attach — so zmx never sees its detach byte and `Ctrl+\` silently does
  nothing. This bit the head-chef entry (Claude Code enables modifyOtherKeys) while
  idle line cooks detached fine. `run_attach_restoring_detach` in `backend/real.rs`
  mitigates it by disabling those modes right after zmx's replay so `Ctrl+\` reaches
  zmx raw again (tradeoff: the app loses enhanced key disambiguation while focused).
  The clean fix belongs in zmx — its detach handler should also recognize the encoded
  `Ctrl+\`, or not replay input-only keyboard modes to the client.

## Home directory

Defaults to `~/yeschef`; overridden with `YESCHEF_HOME` (used by e2e tests for isolation).
Layout:

```
~/yeschef/
  yeschef.db
  AGENTS.md         # kitchen manual, refreshed by `init`
  projects/
    <project>/
      .bare/        # bare git clone
      worktrees/
        <branch>/   # git worktree (one per ticket)
```
