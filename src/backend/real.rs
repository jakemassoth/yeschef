use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use std::path::PathBuf;

use super::{BranchStatus, GitBackend, TmuxBackend, WindowInfo};

// ---------------------------------------------------------------------------
// GitBackend — wraps `git`
// ---------------------------------------------------------------------------

pub struct RealGitBackend;

impl GitBackend for RealGitBackend {
    fn clone_bare(&self, url: &str, dest: &Path) -> Result<()> {
        let status = Command::new("git")
            .args(["clone", "--bare", url])
            .arg(dest)
            .status()
            .context("failed to run 'git clone --bare'")?;
        if !status.success() {
            bail!("git clone --bare failed for '{url}'");
        }
        Ok(())
    }

    fn set_config(&self, repo: &Path, key: &str, value: &str) -> Result<()> {
        let status = Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["config", key, value])
            .status()
            .context("failed to run 'git config'")?;
        if !status.success() {
            bail!("git config {key} {value} failed");
        }
        Ok(())
    }

    fn unset_config(&self, repo: &Path, key: &str) -> Result<()> {
        // `git config --unset` exits 5 when the key is missing — treat that as success.
        let output = Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["config", "--unset", key])
            .output()
            .context("failed to run 'git config --unset'")?;
        if output.status.success() {
            return Ok(());
        }
        if output.status.code() == Some(5) {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git config --unset {} failed: {}", key, stderr.trim());
    }

    fn add_worktree(
        &self,
        bare_repo: &Path,
        worktree_path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<()> {
        // Check whether the branch already exists in the repo.
        // If it does, check it out directly; if not, create it from base.
        let branch_exists = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args(["rev-parse", "--verify", branch])
            .output()
            .is_ok_and(|o| o.status.success());

        let mut cmd = Command::new("git");
        cmd.args(["-C"]).arg(bare_repo).arg("worktree").arg("add");
        if branch_exists {
            cmd.arg(worktree_path).arg(branch);
        } else {
            cmd.args(["-b", branch]).arg(worktree_path).arg(base);
        }

        let status = cmd.status().context("failed to run 'git worktree add'")?;
        if !status.success() {
            bail!("git worktree add failed for branch '{branch}' from '{base}'");
        }
        Ok(())
    }

    fn remove_worktree(&self, bare_repo: &Path, worktree_path: &Path) -> Result<()> {
        // `git worktree remove --force` drops the worktree and its admin files.
        // Fall back to pruning if the directory is already gone.
        let status = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args(["worktree", "remove", "--force"])
            .arg(worktree_path)
            .status()
            .context("failed to run 'git worktree remove'")?;
        if !status.success() {
            // Best-effort: prune stale worktree metadata so the registry recovers.
            let _ = Command::new("git")
                .args(["-C"])
                .arg(bare_repo)
                .args(["worktree", "prune"])
                .status();
        }
        Ok(())
    }

    fn ensure_tracking_refspec(&self, bare_repo: &Path) -> Result<()> {
        // `--replace-all` collapses any existing fetch refspecs down to the
        // single remote-tracking one, so a bare clone (which has none) and a
        // mirror-style clone (`+refs/heads/*:refs/heads/*`) both end up correct.
        let status = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args([
                "config",
                "--replace-all",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ])
            .status()
            .context("failed to run 'git config remote.origin.fetch'")?;
        if !status.success() {
            bail!("git config remote.origin.fetch failed");
        }
        Ok(())
    }

    fn delete_branch(&self, bare_repo: &Path, branch: &str) -> Result<()> {
        // `git branch -D` force-deletes the local branch. The worktree is
        // removed before this runs, so the branch isn't checked out anywhere.
        let output = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args(["branch", "-D", branch])
            .output()
            .context("failed to run 'git branch -D'")?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        // A branch that's already gone is not an error for teardown.
        if stderr.contains("not found") {
            return Ok(());
        }
        bail!("git branch -D {} failed: {}", branch, stderr.trim());
    }

    fn fetch_prune(&self, bare_repo: &Path) -> Result<()> {
        // Callers run `ensure_tracking_refspec` first, so the fetch refspec is
        // configured and `--prune` maintains `refs/remotes/origin/*`.
        let status = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args(["fetch", "--prune", "origin"])
            .status()
            .context("failed to run 'git fetch --prune origin'")?;
        if !status.success() {
            bail!("git fetch --prune origin failed");
        }
        Ok(())
    }

    fn branch_status(
        &self,
        bare_repo: &Path,
        branch: &str,
        main_ref: &str,
    ) -> Result<BranchStatus> {
        // Merged: the branch tip is an ancestor of the main line, so its work
        // is already in `main`. This is the definitive safe-to-reap signal.
        let merged = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args(["merge-base", "--is-ancestor", branch, main_ref])
            .status()
            .is_ok_and(|s| s.success());
        if merged {
            return Ok(BranchStatus::Merged);
        }

        // Gone: the configured upstream was deleted on the remote (surfaced by
        // `fetch --prune`). `%(upstream:track)` reports `[gone]` in that case.
        // This catches squash/rebase merges, whose tips never become ancestors
        // of `main` but whose branch was cleaned up after the PR landed.
        let output = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args([
                "for-each-ref",
                "--format=%(upstream:track)",
                &format!("refs/heads/{branch}"),
            ])
            .output()
            .context("failed to run 'git for-each-ref'")?;
        if String::from_utf8_lossy(&output.stdout).contains("[gone]") {
            return Ok(BranchStatus::Gone);
        }

        Ok(BranchStatus::Unmerged)
    }

    fn default_branch(&self, bare_repo: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["-C"])
            .arg(bare_repo)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .context("failed to run 'git symbolic-ref'")?;
        if !output.status.success() {
            // Fall back to checking remote/HEAD
            let output2 = Command::new("git")
                .args(["-C"])
                .arg(bare_repo)
                .args(["remote", "show", "origin"])
                .output()
                .context("failed to run 'git remote show origin'")?;
            let text = String::from_utf8_lossy(&output2.stdout);
            for line in text.lines() {
                if line.trim().starts_with("HEAD branch:") {
                    let branch = line.split(':').nth(1).unwrap_or("main").trim().to_string();
                    return Ok(branch);
                }
            }
            return Ok("main".to_string());
        }
        let branch = String::from_utf8(output.stdout)
            .context("invalid UTF-8 in git output")?
            .trim()
            .to_string();
        if branch.is_empty() {
            Ok("main".to_string())
        } else {
            Ok(branch)
        }
    }
}

// ---------------------------------------------------------------------------
// TmuxBackend — wraps `tmux` (session attach/detach for the terminal)
// ---------------------------------------------------------------------------
//
// The head-chef trait is window-oriented (one `yeschef` brigade holding many
// ticket windows). tmux has real windows, but yeschef maps each ticket window
// onto its own standalone tmux session named `yeschef-<window>` so tickets are
// fully isolated: each has an independent lifecycle and detaches on its own
// without disturbing the others. `session_exists`/`list_windows` then derive
// the brigade's state from the set of `yeschef-…` sessions.
//
// Every invocation runs against a private tmux server: a dedicated `-L` socket
// with yeschef's own config file (`-f`). That keeps yeschef's sessions off the
// user's default tmux server and stops it from reading or clobbering their
// `~/.tmux.conf`. The config ships the `extended-keys`/`terminal-features`
// settings that let Claude Code see Shift+Enter (see `config::ensure_tmux_conf`).
//
// The socket name is configurable via `YESCHEF_TMUX_SOCKET` (default `yeschef`),
// resolved once per backend in `new`. Production leaves it unset and runs on
// `yeschef`; the e2e tests point it at a throwaway per-test socket so they can
// create and kill sessions — even a whole `kill-server` — without ever touching
// the operator's live `yeschef` server.

/// Default `-L` socket name for yeschef's private tmux server, used when
/// `YESCHEF_TMUX_SOCKET` is unset. Sessions on this socket are isolated from
/// the user's default (`~/.tmux`) server.
pub const DEFAULT_TMUX_SOCKET: &str = "yeschef";

/// The env var that overrides the tmux `-L` socket name.
pub const TMUX_SOCKET_ENV: &str = "YESCHEF_TMUX_SOCKET";

/// Resolve the tmux `-L` socket name from [`TMUX_SOCKET_ENV`], falling back to
/// [`DEFAULT_TMUX_SOCKET`] when it is unset or empty. This is the single source
/// of truth for which tmux server every yeschef invocation drives — making it
/// configurable is what lets the e2e suite run on a throwaway per-test socket
/// instead of the operator's live `yeschef` server.
pub fn resolve_tmux_socket() -> String {
    match std::env::var(TMUX_SOCKET_ENV) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => DEFAULT_TMUX_SOCKET.to_string(),
    }
}

pub struct RealTmuxBackend {
    /// Path to yeschef's own tmux config, passed via `-f` so the private server
    /// starts with our `extended-keys`/scrollback settings and never sources
    /// the user's `~/.tmux.conf`.
    config_path: PathBuf,
    /// The `-L` socket name this backend drives, resolved once from
    /// [`resolve_tmux_socket`] at construction.
    socket: String,
}

impl RealTmuxBackend {
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            config_path,
            socket: resolve_tmux_socket(),
        }
    }

    /// A `tmux` command pre-wired to yeschef's private server: the dedicated
    /// `-L` socket plus `-f <our config>`. Global flags must precede the tmux
    /// subcommand, so callers append the subcommand and its args after this.
    fn cmd(&self) -> Command {
        let mut c = Command::new("tmux");
        c.arg("-L")
            .arg(&self.socket)
            .arg("-f")
            .arg(&self.config_path);
        c
    }

    /// Launch `command` in a new detached session with the exact id `id`,
    /// rooted at `cwd`. Shared by `new_window` (ticket windows) and
    /// `ensure_raw_session` (the bare head-chef session). Runs through a login
    /// shell so the agent inherits a full environment (PATH, etc.); `-c` sets
    /// the pane's start directory. A generous initial size gives the detached
    /// agent room to draw before a client attaches (tmux would otherwise
    /// default to 80x24) and resizes to the client's size on attach.
    fn launch_detached(&self, id: &str, cwd: &Path, command: &str) -> Result<()> {
        let status = self
            .cmd()
            .args(["new-session", "-d", "-s", id, "-x", "200", "-y", "50", "-c"])
            .arg(cwd)
            .args(["sh", "-lc", command])
            .status()
            .context("failed to run 'tmux new-session'")?;
        if !status.success() {
            bail!("tmux new-session failed for '{id}'");
        }
        Ok(())
    }

    /// Capture a session's full scrollback as a VT/ANSI byte stream (SGR
    /// colours/attributes preserved via `-e`), addressed by its exact id.
    /// Shared by `capture_pane_styled` and `capture_raw_styled`. tmux joins
    /// captured rows with bare LF; a VT parser reads LF as line-feed-only and
    /// staircases the output, so normalize to CRLF to anchor each row at
    /// column 0. Returned whole, untrimmed (the styled replay is a unit).
    fn capture_styled(&self, id: &str) -> Result<String> {
        let output = self
            .cmd()
            .args(["capture-pane", "-e", "-p", "-S", "-", "-t", id])
            .output()
            .context("failed to run 'tmux capture-pane -e'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "tmux capture-pane -e failed for '{}': {}",
                id,
                stderr.trim()
            );
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        Ok(raw.replace('\n', "\r\n"))
    }

    /// Attach the current terminal to the session with the exact id `id`.
    /// Shared by `attach` and `attach_raw`. `TMUX` is cleared from the child
    /// environment: a caller invoked from inside a yeschef tmux session (e.g. a
    /// line cook running `yeschef tui` on itself) would otherwise have tmux
    /// refuse to nest, or target the caller's own session. Clearing it makes
    /// the explicit `-t` target always win.
    fn attach_id(&self, id: &str) -> Result<()> {
        let status = self
            .cmd()
            .env_remove("TMUX")
            .args(["attach-session", "-t", id])
            .status()
            .context("failed to run 'tmux attach-session'")?;
        if !status.success() {
            bail!("tmux attach-session failed for '{id}'");
        }
        Ok(())
    }

    /// List the names of all sessions on yeschef's private server (one per line
    /// via `list-sessions -F '#{session_name}'`). No server / no sessions
    /// yields an empty list rather than an error.
    fn sessions(&self) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .context("failed to run 'tmux list-sessions'")?;
        if !output.status.success() {
            // No server running yet (or no sessions) — not an error.
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

/// Build the flat tmux session id for a ticket window: each window is its own
/// standalone session, namespaced under the brigade session name.
fn sid(session: &str, window: &str) -> String {
    format!("{session}-{window}")
}

impl TmuxBackend for RealTmuxBackend {
    fn session_exists(&self, session: &str) -> Result<bool> {
        // The brigade "session" exists if any ticket session is registered under it.
        let prefix = format!("{session}-");
        Ok(self.sessions()?.iter().any(|s| s.starts_with(&prefix)))
    }

    fn ensure_session(&self, _session: &str) -> Result<()> {
        // No-op: there is no parent session to hold windows — each ticket window
        // is its own tmux session, created lazily by `new_window`.
        Ok(())
    }

    fn new_window(&self, session: &str, window: &str, cwd: &Path, command: &str) -> Result<()> {
        self.launch_detached(&sid(session, window), cwd, command)
    }

    fn window_exists(&self, session: &str, window: &str) -> Result<bool> {
        Ok(self.list_windows(session)?.iter().any(|w| w.name == window))
    }

    fn send_keys(&self, session: &str, window: &str, text: &str) -> Result<()> {
        let id = sid(session, window);
        // `send-keys -l` sends the text literally (no key-name lookup, so text
        // like "C-c" isn't interpreted); `--` guards leading dashes. Then a
        // separate `Enter` submits it, mirroring how the agent's line editor
        // reads a carriage return.
        let status = self
            .cmd()
            .args(["send-keys", "-t", &id, "-l", "--", text])
            .status()
            .context("failed to run 'tmux send-keys'")?;
        if !status.success() {
            bail!("tmux send-keys failed for '{id}'");
        }
        let status = self
            .cmd()
            .args(["send-keys", "-t", &id, "Enter"])
            .status()
            .context("failed to run 'tmux send-keys' (Enter)")?;
        if !status.success() {
            bail!("tmux send-keys (Enter) failed for '{id}'");
        }
        Ok(())
    }

    fn capture_pane(&self, session: &str, window: &str, lines: Option<usize>) -> Result<String> {
        let id = sid(session, window);
        // `capture-pane -p -S -` dumps the full scrollback (from the start of
        // history) as de-styled text; trim to the last N lines ourselves, as
        // tmux has no last-N flag.
        let output = self
            .cmd()
            .args(["capture-pane", "-p", "-S", "-", "-t", &id])
            .output()
            .context("failed to run 'tmux capture-pane'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tmux capture-pane failed for '{}': {}", id, stderr.trim());
        }
        let full = String::from_utf8_lossy(&output.stdout).into_owned();
        // `capture-pane` pads the visible pane to its full height with blank
        // lines, so a session whose output sits near the top is followed by a
        // screenful of blanks. Drop those trailing blanks before trimming to
        // the last N lines — otherwise the N-line window lands entirely on
        // padding and hides the real output (and `status`'s last-line probe
        // comes up empty). zmx's `history` never padded, so this restores the
        // same "last N lines of actual output" behaviour.
        let all: Vec<&str> = full.lines().collect();
        let end = all
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map_or(0, |i| i + 1);
        let content = &all[..end];
        let tail = match lines {
            Some(n) => &content[content.len().saturating_sub(n)..],
            None => content,
        };
        let mut text = tail.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        Ok(text)
    }

    fn capture_pane_styled(&self, session: &str, window: &str) -> Result<String> {
        self.capture_styled(&sid(session, window))
    }

    fn list_windows(&self, session: &str) -> Result<Vec<WindowInfo>> {
        // Recover ticket windows from the set of `yeschef-…` sessions. A
        // finished ticket's session is destroyed when its agent exits (tmux's
        // default), so a vanished ticket surfaces as "gone" rather than "dead";
        // both liveness flags stay false, matching `run_status`'s expectations.
        let prefix = format!("{session}-");
        let windows = self
            .sessions()?
            .into_iter()
            .filter_map(|s| {
                s.strip_prefix(&prefix).map(|name| WindowInfo {
                    name: name.to_string(),
                    active: false,
                    dead: false,
                })
            })
            .collect();
        Ok(windows)
    }

    fn kill_window(&self, session: &str, window: &str) -> Result<()> {
        let id = sid(session, window);
        let output = self
            .cmd()
            .args(["kill-session", "-t", &id])
            .output()
            .context("failed to run 'tmux kill-session'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // A session that's already gone is not an error for teardown.
            if stderr.contains("can't find") || stderr.contains("no such") {
                return Ok(());
            }
            bail!("tmux kill-session failed for '{}': {}", id, stderr.trim());
        }
        Ok(())
    }

    fn attach(&self, session: &str, window: Option<&str>) -> Result<()> {
        // With a ticket selected, attach to that ticket's session directly;
        // otherwise fall back to the first live ticket session in the brigade.
        let id = if let Some(w) = window {
            sid(session, w)
        } else {
            let prefix = format!("{session}-");
            self.sessions()?
                .into_iter()
                .find(|s| s.starts_with(&prefix))
                .ok_or_else(|| anyhow::anyhow!("no live yeschef sessions to attach to"))?
        };
        self.attach_id(&id)
    }

    // ---- Bare-session (raw id) operations --------------------------------
    //
    // Twins of `new_window` / `capture_pane_styled` / `attach` that target a
    // raw session id with no `sid` namespacing — used for the TUI's pinned
    // head-chef session. They share the same private-server helpers
    // (`launch_detached` / `capture_styled` / `attach_id`) as their namespaced
    // counterparts, so the tmux invocation stays identical.

    fn ensure_raw_session(&self, id: &str, cwd: &Path, command: &str) -> Result<()> {
        // Idempotent: leave an existing session running untouched, so
        // re-opening the TUI never restarts or duplicates the head chef.
        if self.sessions()?.iter().any(|s| s == id) {
            return Ok(());
        }
        self.launch_detached(id, cwd, command)
    }

    fn capture_raw_styled(&self, id: &str) -> Result<String> {
        self.capture_styled(id)
    }

    fn attach_raw(&self, id: &str) -> Result<()> {
        self.attach_id(id)
    }
}

// ---------------------------------------------------------------------------
// Utilities for real backend construction
// ---------------------------------------------------------------------------

/// Check that a binary exists and is executable.
pub fn check_binary(name: &str) -> Result<()> {
    let output = Command::new("which")
        .arg(name)
        .output()
        .with_context(|| format!("failed to run 'which {name}': is 'which' available?"))?;
    if !output.status.success() {
        bail!(
            "required dependency '{name}' not found in PATH; please install it before using yeschef"
        );
    }
    Ok(())
}
