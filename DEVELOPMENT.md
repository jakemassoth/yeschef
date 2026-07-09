# Developing yeschef

Guidance for working on the yeschef **source code** itself — building, testing,
architecture. If you were launched to *orchestrate* agents, that's a different role; see
`AGENTS.md`. This file is for when your job is to change yeschef's own code.

## What yeschef does

yeschef is a CLI that orchestrates multiple coding agents in parallel across git
worktrees, using tmux. One head chef agent dispatches a brigade of agents — each on its
own branch, in its own git worktree, inside its own tmux session — then supervises and
steers them. It is agent-agnostic: a line cook is just a command string launched in a
tmux session. Requires only `git` and `tmux` (no containers, no Nix, no macOS
requirement).

The orchestration "brain" is `AGENTS.md` (shipped in the repo root and written to
`~/yeschef/` by `init`); the head chef agent reads it and drives the loop via the CLI.

Workflow:

```
yeschef init
yeschef project add <git-url> [name]
yeschef spawn <project> <branch> -p "<prompt>"   # worktree + tmux session + agent
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

# E2E tests — require git + tmux on PATH (no containers/macOS). Drive real tmux
# sessions on yeschef's private `-L yeschef` server, so run single-threaded.
cargo test --test e2e -- --ignored --test-threads=1
# or via the flake (PATH-checks git + tmux first):
nix run .#e2e
nix run .#e2e -- <test_name>

# Single unit test
cargo test <test_name>
```

The e2e suite is light now (no image builds). It uses unique per-test project names but
shares the global `yeschef` tmux server (a private `-L` socket) — `--test-threads=1`
avoids cross-test races, and each test kills its own tmux session on drop.

## CI — run `nix flake check` before you push

CI is driven entirely by Nix flake checks. Run the whole sandboxed suite locally
**before pushing** so CI doesn't fail after the fact:

```bash
nix flake check        # fmt (rustfmt) + nixfmt + lint (clippy) + unit tests
nix run .#e2e          # the e2e suite (run separately — see below)
```

`checks` in `flake.nix` covers **fmt** (`cargo fmt --check`), **nixfmt**
(nixfmt-rfc-style on `flake.nix`), **lint** (strict clippy), and **test** (unit
tests). The **e2e** suite is deliberately *not* a flake check: it drives real
tmux sessions and real git worktrees (impure, shares the global `yeschef` tmux
server), so it runs un-sandboxed via `nix run .#e2e`. Run both before pushing.

The GitHub Action (`.github/workflows/ci.yml`) runs exactly these two commands on
every push and PR, on an `ubuntu-latest` runner. tmux is a cross-platform nixpkgs
package and the suite has no macOS-specific behaviour, so there is no macOS
requirement (the old zmx backend was Apple-SDK-coupled — that constraint is gone).
It installs Nix with the
[Determinate Nix action](https://github.com/determinatesystems/determinate-nix-action).

## Verifying changes

Type-checking is not verification. Before declaring a change done, run the tests that
actually exercise it:

- Touching `store`/`names`/orchestration logic reachable from mocks → `cargo test --bin yeschef`.
- Touching the real tmux/git backends or command wiring from `main.rs` → run the relevant
  e2e test (`cargo test --test e2e -- --ignored --test-threads=1 <name>`). The e2e tests
  are the only thing that exercises real `tmux`/`git` behavior.
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
- `TmuxBackend` — wraps `tmux` (session lifecycle via
  `new-session`/`send-keys`/`capture-pane`/`list-sessions`/`kill-session`/`attach-session`)

`src/backend/real.rs` has the real implementations; `src/backend/mock.rs` has recording
mocks (the `TmuxBackend` mock tracks an in-memory window list, so orchestration logic is
unit-testable with no tmux). `Config` in `src/config.rs` holds both backends as
`Box<dyn Trait>` plus the `Store`, and is constructed in `main.rs` before dispatch.

Command logic lives in `src/commands/`:
- `init.rs` — creates `~/yeschef/` layout, writes `AGENTS.md`, validates `git` + `tmux`.
- `project.rs` — `add` (bare clone + worktrees dir) and `list`.
- `orchestrate.rs` — `spawn`, `send`, `peek`, `status`, `kill`, `attach`. `spawn` is the
  meaty one: creates the worktree (guarded by `RollbackGuard`), opens a tmux session
  running the agent at the worktree, and registers the ticket in SQLite.
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
(`yeschef_session()`), which namespaces every ticket's tmux session; each ticket's window is
`<project>-<sanitized-branch>` (`window_name`), embedded into the tmux session id
`yeschef-<window>`. Branch sanitization strips `.`/`:` (tmux `-t` target separators) so the
derived name stays a clean tmux session id.

## tmux backend notes

- **Session-per-ticket.** tmux has real windows, but yeschef maps each ticket "window"
  onto its own standalone tmux session named `yeschef-<window>` (`sid()` in
  `backend::real`). This keeps tickets fully isolated: each has an independent lifecycle
  and detaches on its own without disturbing the others. `session_exists`/`list_windows`
  derive the brigade's state from the set of `yeschef-…` sessions; there is no parent
  session to pre-create, so `ensure_session` is a no-op.
- **Private server.** Every `tmux` invocation runs against a dedicated `-L` socket
  (`TMUX_SOCKET = "yeschef"`) loaded with yeschef's own config (`tmux -f <home>/tmux.conf`),
  so yeschef's sessions never touch the user's default tmux server or `~/.tmux.conf`. The
  config is baked into the binary (`include_str!("../tmux.conf")`) and rewritten to
  `<home>/tmux.conf` on every `Config::load` (see `config::ensure_tmux_conf`).
- **Shift+Enter.** `tmux.conf` sets `extended-keys on` + `terminal-features
  'xterm*:extkeys'` so tmux forwards CSI-u / modifyOtherKeys sequences to the running
  agent — that is what lets Claude Code see Shift+Enter as a distinct key (insert newline)
  rather than a plain Enter (submit). The outer terminal must itself support CSI-u
  (iTerm2, Ghostty, kitty, WezTerm, VS Code terminal all do).
- **send_keys** writes the literal text with `send-keys -l -- <text>` (no key-name
  lookup), then sends a separate `Enter` to submit it.
- **capture_pane** dumps the full scrollback with `capture-pane -p -S -`, then trims to
  the last N lines — after stripping the blank lines tmux uses to pad the visible pane to
  its full height (otherwise a last-N window lands on padding and hides output at the top).
- **capture_pane_styled** (the TUI's live preview) uses `capture-pane -e -p -S -` to keep
  SGR colours, then normalizes tmux's bare-LF row separators to CRLF — a VT parser reads a
  lone LF as a line-feed only and would staircase the rows otherwise.
- **Liveness.** tmux destroys a session when its agent process exits (no `remain-on-exit`),
  so a finished ticket's session simply disappears — it surfaces as "gone" in `status`,
  never "dead". Both `WindowInfo` liveness flags stay false.
- **Detach.** The TUI hands the real terminal to `tmux attach-session` on `Enter`; the
  human detaches with tmux's own `Ctrl+b d`, which returns cleanly to the TUI. (Fixing the
  broken detach of the old zmx backend is what motivated the switch to tmux.) `attach`
  clears `TMUX` from the child env so an explicit `-t` target always wins even when yeschef
  is invoked from inside a yeschef tmux session.

## Home directory

Defaults to `~/yeschef`; overridden with `YESCHEF_HOME` (used by e2e tests for isolation).
Layout:

```
~/yeschef/
  yeschef.db
  tmux.conf         # yeschef's own tmux config, loaded via `tmux -f`
  AGENTS.md         # kitchen manual, refreshed by `init`
  projects/
    <project>/
      .bare/        # bare git clone
      worktrees/
        <branch>/   # git worktree (one per ticket)
```
