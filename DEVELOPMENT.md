# Developing yeschef

Guidance for working on the yeschef **source code** itself — building, testing,
architecture. If you were launched to *orchestrate* agents, that's a different role; see
`AGENTS.md`. This file is for when your job is to change yeschef's own code.

## What yeschef does

yeschef is a CLI that orchestrates multiple coding agents in parallel across git
worktrees, using tmux. One head chef agent dispatches a brigade of agents — each on its
own branch, in its own git worktree, inside its own window of a shared `yeschef` tmux
session — then supervises and steers them. It is agent-agnostic: a line cook is just a
command string launched in a tmux window. Requires only `git` and `tmux` (no containers,
no Nix, no macOS requirement).

The orchestration "brain" is `AGENTS.md` (shipped in the repo root and written to
`~/yeschef/` by `init`); the head chef agent reads it and drives the loop via the CLI.

Workflow:

```
yeschef init
yeschef project add <git-url> [name]
yeschef spawn <project> <branch> -p "<prompt>"   # worktree + tmux window + agent
yeschef send  <project> <branch> "<one-line steer>"
yeschef peek  <project> <branch>
yeschef status
yeschef tui                                      # attach to the brigade tab bar
yeschef attach [<project> <branch>]
yeschef restart                                  # restart every running agent in place, resuming its conversation
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
# sessions on a throwaway per-TEST `-L` socket (never the live `yeschef`
# server), so they run safely under cargo's default parallel execution.
cargo test --test e2e -- --ignored
# or via the flake (PATH-checks git + tmux first):
nix run .#e2e
nix run .#e2e -- <test_name>

# Single unit test
cargo test <test_name>
```

The e2e suite is light now (no image builds). It uses unique per-test project names and a
**throwaway per-test tmux socket** — each `TestEnv` mints its own unique `-L` name
(PID + nanos + a process-wide atomic counter), exports it as `YESCHEF_TMUX_SOCKET` so the
spawned `yeschef` binary and the tests' own `tmux` helpers drive that one private server,
and never touches the operator's live `yeschef` server. Because every test has a fully
independent server, the suite runs safely under cargo's **default parallel execution** (no
`--test-threads=1`). Each `TestEnv`'s `Drop` runs `kill-server` on its socket to dispose of
the server automatically on pass, fail, or panic; the socket file lives under the test's
temp dir (via `TMUX_TMPDIR`) so it is cleaned up too — nothing leaks into `/tmp/tmux-*/`.

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
tmux sessions and real git worktrees (impure, though on a throwaway per-test `-L`
socket rather than the live `yeschef` server), so it runs un-sandboxed via
`nix run .#e2e`. Run both before pushing.

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
  e2e test (`cargo test --test e2e -- --ignored <name>`). The e2e tests are the only thing
  that exercises real `tmux`/`git` behavior.
- Touching a single e2e test → run that specific test, not the whole suite.

## Recording the terminal (demos / repros)

The dev shell ships `vhs` (charmbracelet/vhs) so you can record a terminal
session headlessly and attach a gif/mp4 to a PR. The `terminal-recording` skill
(`.claude/skills/terminal-recording/SKILL.md`) is the quick-reference. A worked
example lives in `docs/tui-demo.tape` (records `yeschef tui` — the native tmux
tab bar — into `docs/tui-demo.gif`, standing up a throwaway kitchen off-camera
via `docs/tui-demo-setup.sh`); regenerate it with `nix develop --command vhs
docs/tui-demo.tape`.

## Architecture

External I/O is behind two traits in `src/backend/mod.rs`:
- `GitBackend` — wraps `git` (bare clone, worktree add/remove, config, default branch)
- `TmuxBackend` — wraps `tmux`. One `yeschef` session holds the head chef (window 0) and
  one window per cook, driven via
  `new-session`/`new-window`/`send-keys`/`capture-pane`/`set-window-option`/`list-windows`/`kill-window`/`attach-session`.

`src/backend/real.rs` has the real implementations; `src/backend/mock.rs` has recording
mocks (the `TmuxBackend` mock tracks an in-memory window list, so orchestration logic is
unit-testable with no tmux). `Config` in `src/config.rs` holds both backends as
`Box<dyn Trait>` plus the `Store`, and is constructed in `main.rs` before dispatch.

Command logic lives in `src/commands/`:
- `init.rs` — creates `~/yeschef/` layout, writes `AGENTS.md`, validates `git` + `tmux`.
- `project.rs` — `add` (bare clone + worktrees dir) and `list`.
- `orchestrate.rs` — `spawn`, `send`, `peek`, `status`, `kill`, `attach`, `tui`, `restart`.
  `spawn` is the meaty one: ensures the brigade session exists (head chef at window 0),
  creates the worktree (guarded by `RollbackGuard`), opens a tmux window running the agent
  at the worktree, and registers the ticket in SQLite. `tui` is ~3 lines: ensure the
  brigade session, then `tmux attach` — the native tab bar is the whole UI (see the tmux
  notes). `restart` swaps every live agent's process for a fresh one **in place** (tmux
  `respawn-pane -k`, so the window/tab/worktree survive), resuming its prior conversation
  (`claude --continue` for claude-family agents; other agents restart verbatim). It walks
  the ticket registry intersected with the live window list, respawning cooks first and the
  head chef last — so a `restart` issued from the head chef's own window, which respawning
  window 0 would kill, still gets every cook back up first. It's the "pick up a Claude Code
  update without losing context" button.
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
(`yeschef_session()`), the single tmux session holding every window; the head chef is
window 0 (`headchef_window()`) and each ticket's window is `<project>-<sanitized-branch>`
(`window_name`), addressed as the tmux target `yeschef:<window>`. Branch sanitization
strips `.`/`:` (tmux `-t` target separators) so the derived name stays a clean tmux
target — and always joins with `-`, so a cook window can never collide with the bare
`headchef` name.

## tmux backend notes

- **One session, real windows.** The whole brigade lives in a single `yeschef` tmux
  session: the head chef at window 0 and one real tmux window per cook, addressed as
  `yeschef:<window>` (`target()` in `backend::real`). This is what lets `tmux attach`
  render every cook as a native tab (the yeschef TUI). `ensure_session` creates the
  session detached with the head chef as window 0 and is idempotent (an existing session,
  head chef and all, is left untouched); `new_window` adds a cook window; `list_windows`
  runs `list-windows -t yeschef`. Killing a cook is `kill-window` (per-cook), so tearing
  one down never disturbs the others or the head chef.
- **Private server.** Every `tmux` invocation runs against a dedicated `-L` socket loaded
  with yeschef's own config (`tmux -f <home>/tmux.conf`), so yeschef's session never touches
  the user's default tmux server or `~/.tmux.conf`. The socket name is resolved once per
  backend by `backend::real::resolve_tmux_socket` — `YESCHEF_TMUX_SOCKET` if set, else the
  `DEFAULT_TMUX_SOCKET` (`"yeschef"`). Production leaves it unset; the e2e tests point it at
  a throwaway per-test socket so their `kill-window`/`kill-server` calls can never reach the
  operator's live `yeschef` server. The config is baked into the binary
  (`include_str!("../tmux.conf")`) and rewritten to `<home>/tmux.conf` on every
  `Config::load` (see `config::ensure_tmux_conf`).
- **The TUI is the tmux status line.** `tmux.conf` ships a `window-status-format` that
  renders each tab from the per-window `@status` user option — a glyph + colour, live, with
  no polling and no rendering code of ours (CHEF ★ magenta · IN_PROGRESS ● yellow · DONE ✓
  green · BLOCKED ■ red · NEW ○ grey). `set_window_status` (one `set-window-option`) is
  called from `run_ticket_status_set` on every `ticket ... status-set`, so an attached tab
  bar recolours the instant a cook reports. The window *name* stays the stable ticket id
  (the send/peek/kill target); status decoration lives only in `@status`, so they never
  collide.
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
- **Liveness.** tmux closes a window when its agent process exits (no `remain-on-exit`),
  so a finished ticket's window simply drops out of `list-windows` — it surfaces as "gone"
  in `status`, never "dead".
- **Restart in place.** `respawn_window` runs `respawn-pane -k -c <cwd>` to kill a pane's
  current process and relaunch a command in the *same* pane. The window (its name, tab
  position, and `@status` colour) is untouched — unlike kill + `new-window`, which would
  drop the tab and lose its decoration. This is what `restart` uses to swap a running agent
  for a fresh one without disturbing the brigade layout.
- **Detach.** `yeschef tui` (and `yeschef attach`) hand the real terminal to
  `tmux attach-session`; the human switches cooks with `prefix+n`/`p`/`<n>` (or the
  `prefix+w` tree), jumps to the head chef with `prefix+0`, and detaches cleanly with
  tmux's own `prefix+d`. (Fixing the broken detach of the old zmx backend is what motivated
  the switch to tmux.) `attach` clears `TMUX` from the child env so an explicit `-t` target
  always wins even when yeschef is invoked from inside a yeschef tmux session.

## Home directory

Defaults to `~/yeschef`; overridden with `YESCHEF_HOME` (used by e2e tests for isolation).
A second env var, `YESCHEF_TMUX_SOCKET`, overrides the tmux `-L` socket name (default
`yeschef`) — the e2e tests set it to a throwaway per-test name so they never touch the
operator's live tmux server. Layout:

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
