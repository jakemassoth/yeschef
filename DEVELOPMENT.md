# Developing yeschef

Guidance for working on the yeschef **source code** itself — building, testing,
architecture. If you were launched to *orchestrate* agents, that's a different role; see
`AGENTS.md`. This file is for when your job is to change yeschef's own code.

## What yeschef does

yeschef is a CLI that orchestrates multiple coding agents in parallel across git
worktrees, using [herdr](https://github.com/ogulcancelik/herdr) (an "agent
multiplexer") as its terminal/session layer. One head chef agent dispatches a brigade of
agents — each on its own branch, in its own git worktree, inside its own herdr **workspace**
in a shared `yeschef` herdr **session** — then supervises and steers them. It is
agent-agnostic: a line cook is just a command string launched in a herdr pane. Requires
only `git` and `herdr` (no containers, no macOS requirement).

herdr replaced an earlier tmux-based UI (which itself replaced a custom ratatui TUI). The
migration rationale, capability mapping, and the "why herdr" writeup live in
`docs/herdr-investigation.md`.

The orchestration "brain" is `AGENTS.md` (shipped in the repo root and written to
`~/yeschef/` by `init`); the head chef agent reads it and drives the loop via the CLI.

Workflow:

```
yeschef init
yeschef project add <git-url> [name]
yeschef spawn <project> <branch> -p "<prompt>"   # worktree + herdr workspace + agent
yeschef send  <project> <branch> "<one-line steer>"
yeschef peek  <project> <branch>
yeschef status
yeschef tui                                      # attach to herdr's native brigade UI
yeschef attach [<project> <branch>]
yeschef restart                                  # bounce the herdr server; workspaces restored, agents resumed
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

# E2E tests — require git + herdr on PATH. Drive a real herdr server on a
# throwaway per-TEST session + config home (never the live `yeschef` brigade),
# so they run safely under cargo's default parallel execution.
cargo test --test e2e -- --ignored
# or via the flake (puts git + herdr on PATH first):
nix run .#e2e
nix run .#e2e -- <test_name>

# Single unit test
cargo test <test_name>
```

The e2e suite uses unique per-test project names and a **throwaway per-test herdr
session** — each `TestEnv` mints its own unique session name (PID + nanos + a process-wide
atomic counter), exports it as `YESCHEF_HERDR_SESSION`, and points `XDG_CONFIG_HOME` at a
private dir under the test's temp dir (herdr derives its socket path, session state, and
logs from there). So the spawned `yeschef` binary, the detached herdr server it starts, and
the tests' own `herdr` helpers all drive that one private session, never the operator's
live `yeschef` brigade. Because every test has a fully independent session + config home,
the suite runs safely under cargo's **default parallel execution** (no `--test-threads=1`).
Each `TestEnv`'s `Drop` runs `herdr server stop` on its session to dispose of the detached
server on pass, fail, or panic; the config home (socket, session json, logs) lives under
the test's temp dir, so it is removed with it — nothing leaks. (The temp dir is rooted at
`/tmp` because herdr's unix socket path has a hard ~104-byte `sun_path` limit that macOS's
deep default temp dir would overflow.)

## CI — run `nix flake check` before you push

CI is driven entirely by Nix flake checks. Run the whole sandboxed suite locally
**before pushing** so CI doesn't fail after the fact:

```bash
nix flake check        # fmt (rustfmt) + nixfmt + lint (clippy) + unit tests
nix run .#e2e          # the e2e suite (run separately — see below)
```

`checks` in `flake.nix` covers **fmt** (`cargo fmt --check`), **nixfmt**
(nixfmt-rfc-style on `flake.nix`), **lint** (strict clippy), and **test** (unit
tests) — all built from source, so `nix flake check` never has to compile herdr. The
**e2e** suite is deliberately *not* a flake check: it drives a real herdr server and real
git worktrees (impure, though on a throwaway per-test session rather than the live
`yeschef` brigade), so it runs un-sandboxed via `nix run .#e2e`, which puts herdr on PATH.
Run both before pushing.

The GitHub Action (`.github/workflows/ci.yml`) runs exactly these two commands on
every push and PR, on an `ubuntu-latest` runner. herdr comes from its own pinned flake and
the suite has no macOS-specific behaviour, so there is no macOS requirement.
It installs Nix with the
[Determinate Nix action](https://github.com/determinatesystems/determinate-nix-action).

## Verifying changes

Type-checking is not verification. Before declaring a change done, run the tests that
actually exercise it:

- Touching `store`/`names`/orchestration logic reachable from mocks → `cargo test --bin yeschef`.
- Touching the real herdr/git backends or command wiring from `main.rs` → run the relevant
  e2e test (`cargo test --test e2e -- --ignored <name>`). The e2e tests are the only thing
  that exercises real `herdr`/`git` behavior.
- Touching a single e2e test → run that specific test, not the whole suite.

## Recording the terminal (demos / repros)

The dev shell ships `vhs` (charmbracelet/vhs) so you can record a terminal
session headlessly and attach a gif/mp4 to a PR. The `terminal-recording` skill
(`.claude/skills/terminal-recording/SKILL.md`) is the quick-reference. (The old
tmux-based demo tapes were removed with the tmux backend; a fresh `yeschef tui`
recording of herdr's UI is a nice follow-up but not required.)

## Architecture

External I/O is behind two traits in `src/backend/mod.rs`:
- `GitBackend` — wraps `git` (bare clone, worktree add/remove, config, default branch,
  fetch/prune, and the merged/gone branch classification `cleanup` needs). **Unchanged by
  the herdr migration** — herdr has no project/git model, so this all stays.
- `HerdrBackend` — wraps the `herdr` CLI. One `yeschef` herdr session holds the head chef
  and one workspace per cook, driven via
  `ensure_server`/`create_workspace`/`run_in_pane`/`read_pane`/`list_workspaces`/`close_workspace`/`set_display_status`/`attach`
  (each a `herdr … workspace/pane/server` subcommand whose JSON is parsed with serde).

`src/backend/real.rs` has the real implementations; `src/backend/mock.rs` has recording
mocks (the `HerdrBackend` mock tracks an in-memory workspace/pane model, so orchestration
logic is unit-testable with no herdr server). `Config` in `src/config.rs` holds both
backends as `Box<dyn Trait>` plus the `Store`, and is constructed in `main.rs` before
dispatch.

Command logic lives in `src/commands/`:
- `init.rs` — creates `~/yeschef/` layout, writes `AGENTS.md`, validates `git` + `herdr`.
- `project.rs` — `add` (bare clone + worktrees dir) and `list`.
- `orchestrate.rs` — `spawn`, `send`, `peek`, `status`, `kill`, `attach`, `tui`, `restart`.
  `spawn` is the meaty one: ensures the brigade (server + pinned head chef) exists, creates
  the worktree (guarded by `RollbackGuard`), creates a herdr workspace rooted at the
  worktree, launches the agent into its root pane (`run_in_pane`), and registers the ticket
  (with the herdr `workspace_id` + `pane_id`) in SQLite. If launching or registering fails,
  the freshly-created workspace is closed so a retry is clean. `tui` is ~3 lines: ensure the
  brigade, then hand the terminal to `herdr` (its native UI is the whole TUI). `restart`
  bounces the herdr server (`stop_server` then `ensure_server`); herdr persists the session
  to disk, so the workspaces come back and — with `resume_agents_on_restore`, on by default
  — supported agents resume their conversation. herdr owns the resume, so yeschef no longer
  special-cases `claude --continue`. It's the "pick up a Claude Code update without losing
  context" button.
  The `-p` prompt is **never inlined** on the launch command line — a long prompt would
  overflow the OS arg-length limit and the agent harness, treating the giant positional
  arg as a path, dies with `ENAMETOOLONG`. Instead `spawn` writes the prompt to
  `~/yeschef/prompts/<project>-<sanitized-branch>.md` (a stable path outside the worktree,
  so it can't be committed; overwritten on re-spawn) and launches the agent with a short
  `Read the ticket brief at <abs-path> and carry it out start to finish.` instruction. This
  is always-file (no size threshold — simpler, and correct for every prompt length) and
  agent-agnostic, since every agent takes an initial instruction as its positional arg.

State is still persisted in SQLite (`~/yeschef/yeschef.db`, via `src/store.rs`) — the
migration kept it as yeschef's durable ticket ledger. Two tables: `projects` (name,
git_url) and `branches` — the ticket registry — (project, branch, sanitized,
`workspace_id`, `pane_id`, agent, status). The `workspace_id`/`pane_id` are the herdr ids
yeschef addresses the ticket by; matching `workspace_id` against `herdr workspace list` is
how liveness (running vs. gone) is determined. (`docs/herdr-investigation.md` discusses a
possible future SQLite drop; that is explicitly out of scope here.)

`src/guard.rs` is a LIFO rollback guard used in `run_spawn` to undo a partial worktree if
a later step fails.

`src/names.rs` holds naming conventions: `headchef_label()` (`"headchef"`) labels the
pinned head-chef workspace, and `workspace_label(project, branch)` (`<project>/<branch>`)
labels each cook workspace — human-facing labels shown in herdr's UI. yeschef matches a
ticket to its workspace by the stored `workspace_id`, not by label, so labels need not be
unique (herdr accepts `/`). `sanitize_branch` survives only to name the per-ticket prompt
file.

## herdr backend notes

- **One named session.** The whole brigade lives in a single named herdr session
  (`resolve_herdr_session`, default `yeschef`), served by one background herdr **server**.
  Each cook is a herdr **workspace** (`create_workspace` → `herdr workspace create --cwd
  <worktree> --label <project>/<branch>`), whose root **pane** runs the agent; the head
  chef is another workspace. `list_workspaces` (`herdr workspace list`, JSON) is how the
  brigade view and liveness are built; `close_workspace` (`herdr workspace close`, idempotent)
  tears one cook down without disturbing the others.
- **Session isolation.** Naming the session (`--session yeschef` on every invocation) is
  what isolates yeschef's brigade from a human's own default `herdr` session — the analog
  of the old tmux private `-L` socket. The name is resolved once per backend by
  `backend::real::resolve_herdr_session` — `YESCHEF_HERDR_SESSION` if set, else
  `DEFAULT_HERDR_SESSION` (`"yeschef"`). Production leaves it unset; the e2e tests point it
  at a throwaway per-test session (plus a private `XDG_CONFIG_HOME`, since herdr derives its
  socket at `$XDG_CONFIG_HOME/herdr/sessions/<session>/herdr.sock`) so their
  `server stop`/`workspace close` calls never reach the operator's live brigade.
- **Server lifecycle.** `ensure_server` checks `herdr status server`; if the server isn't
  up it spawns `herdr … server` **detached** — `setsid` into a new session, stdio to
  `/dev/null` — so the server (and the agents inside it) outlive the one-shot yeschef
  process and survive the invoking terminal's SIGHUP, then polls until it accepts
  connections. Unlike tmux, the CLI does **not** auto-start a server, so this ensure step is
  load-bearing before any workspace call.
- **Status is herdr's, task status is yeschef's.** herdr **detects** each agent's live
  lifecycle status (`idle`/`working`/`blocked`/`done`/`unknown`) and colours its own
  sidebar — no `@status` hack or rendering code of ours. yeschef's *self-reported task
  status* (`NEW`/`IN_PROGRESS`/`DONE`/`BLOCKED`, which gates `cleanup`) is a separate,
  higher-level signal: `run_ticket_status_set` persists it to SQLite and best-effort pushes
  it to the pane as **display-only** metadata (`set_display_status` → `herdr pane
  report-metadata --token status=…`), which does not touch herdr's own detection.
- **send / peek.** `run_in_pane` (`herdr pane run <pane> <text>`) types a line and submits
  it (Enter) — used both to launch the agent into a fresh pane and to steer a running one.
  `read_pane` (`herdr pane read <pane> --source recent [--lines N]`) returns the pane's
  recent output as plain text.
- **Liveness.** A cook's workspace persists after its agent exits (the pane's shell stays),
  unlike tmux where the window closed. So `status` reports STATE from herdr's detected
  `agent_status` when the workspace is present, and `gone` only when the `workspace_id` has
  dropped out of `herdr workspace list` entirely (e.g. closed).
- **Restart = server bounce.** `restart` runs `stop_server` then `ensure_server`. herdr
  persists the session shape to disk, so the workspaces are restored on restart and, for
  supported integrations, agents resume via their native resume command. This replaces the
  old per-window `respawn-pane -k`.
- **Attach / detach.** `yeschef tui` (and `yeschef attach`) hand the real terminal to bare
  `herdr --session yeschef`, whose native UI shows every workspace as a live,
  status-coloured entry; `attach` optionally `workspace focus`es a specific cook first. The
  human switches workspaces, jumps to the head chef, and detaches with herdr's own
  keybindings.

## Home directory

Defaults to `~/yeschef`; overridden with `YESCHEF_HOME` (used by e2e tests for isolation).
A second env var, `YESCHEF_HERDR_SESSION`, overrides the herdr session name (default
`yeschef`) — the e2e tests set it to a throwaway per-test name (with a private
`XDG_CONFIG_HOME`) so they never touch the operator's live brigade. yeschef writes no herdr
config of its own; herdr uses its normal `~/.config/herdr`. Layout:

```
~/yeschef/
  yeschef.db
  AGENTS.md         # kitchen manual, refreshed by `init`
  prompts/
    <project>-<sanitized-branch>.md   # per-ticket spawn brief (outside any worktree)
  projects/
    <project>/
      .bare/        # bare git clone
      worktrees/
        <branch>/   # git worktree (one per ticket)
```
