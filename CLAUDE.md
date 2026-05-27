# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What nixsand does

nixsand is a macOS aarch64-only CLI that provisions isolated coding agent sandboxes. For each project branch, it creates a git worktree + an Apple container (via the `container` CLI) running a Nix devShell, then attaches via tmux. The intended workflow is:

```
nixsand init
nixsand project add <git-url> [name]
nixsand project branch <project> <branch>
nixsand project attach <project> <branch>
```

## Commands

```bash
# Build
nix build          # or: cargo build

# Lint (clippy -D warnings -D clippy::pedantic)
nix build .#clippy

# Type-check without codegen
nix build .#check

# Unit tests (fast, no external deps)
nix build .#test   # or: cargo test

# E2E tests — requires macOS aarch64 + container CLI + tmux; cannot run in nix sandbox
cargo test --test e2e -- --ignored --test-threads=1
# or via the flake (does PATH checks for container/tmux/git first):
nix run .#e2e
nix run .#e2e -- <test_name>

# Single unit test
cargo test <test_name>

# Single e2e test
cargo test --test e2e -- --ignored --test-threads=1 <test_name>
```

The e2e tests build real container images — ensure at least 15GB free disk before running them. The `nixsand-base` image (~3GB) is kept between runs; per-project images are cleaned up by `ImageCleanup` on test teardown.

## Verifying changes

Type-checking is not verification. Before declaring a change done, run the tests that actually exercise it:

- Touching code reachable from unit tests → `cargo test` (or `nix build .#test`).
- Touching command logic, container/git/tmux orchestration, or anything called from `main.rs` → run the relevant e2e test (`cargo test --test e2e -- --ignored --test-threads=1 <name>`). E2E tests are heavy but they're the only thing that exercises real `container`/`tmux`/`git` behavior.
- Touching a single e2e test → run that specific test, not the whole suite.

If a change can't be tested (e.g. macOS-only behavior from a non-macOS environment), say so explicitly rather than claiming success from `cargo check` alone.

## Architecture

All external I/O is behind three traits in `src/backend/mod.rs`:
- `ContainerBackend` — wraps Apple's `container` CLI
- `GitBackend` — wraps `git`
- `ZmxBackend` — wraps `tmux`

`src/backend/real.rs` has the real implementations; `src/backend/mock.rs` has recording mocks used by unit tests. `Config` in `src/config.rs` holds the three backends as `Box<dyn Trait>` and is constructed in `main.rs` before dispatching to command handlers.

Command logic lives in `src/commands/`:
- `init.rs` — platform check, creates `~/.nixsand/` layout, validates host deps
- `project.rs` — `add`, `list`, `branch`, `attach`. The `branch` command is the meaty one: it creates the git worktree, builds/reuses Docker images, creates and starts the container, and registers everything in SQLite.

State is persisted in a SQLite database (`~/.nixsand/nixsand.db`, opened via `src/store.rs`). The schema has two tables: `projects` (name, git_url, flake_lock_hash) and `branches` (project, branch, sanitized).

`src/image.rs` manages two image layers:
1. `nixsand-base` — NixOS + claude-code (built once, reused across all projects)
2. `nixsand-<project>` — extends base with the project's flake devShell pre-warmed. Rebuilt only when `flake.lock` changes (hash stored in DB).

`src/guard.rs` is a LIFO rollback guard used in `run_branch` to undo partial state (worktree, container) if any step fails.

`src/names.rs` has all naming conventions: container names are `nixsand-<project>-<sanitized-branch>`, tmux sessions are `nixsand_<project>_<sanitized-branch>` (underscores, not dots — tmux parses `.` as window/pane separators in `-t` target args), image tags are `nixsand-base` and `nixsand-<project>`.

## Apple container CLI quirks

These are non-obvious behaviours that differ from Docker:
- `container inspect <name>` returns `[]` with **exit 0** for non-existent containers — cannot be used to check existence. Use `container list --all` instead.
- `container build` requires the context directory as a **positional absolute path** arg (`.arg(context_dir)`). Using `.current_dir()` does not transfer context files.
- `container inspect --format` is not supported.
- `nix-command` and `flakes` experimental features must be written into `/etc/nix/nix.conf` inside the container; passing them on the command line is not reliable in RUN steps.

## Home directory

Defaults to `~/.nixsand`; overridden with `NIXSAND_HOME` env var (used by e2e tests for isolation). Layout:

```
~/.nixsand/
  nixsand.db
  projects/
    <project>/
      .bare/          # bare git clone
      worktrees/
        <branch>/     # git worktree
```
