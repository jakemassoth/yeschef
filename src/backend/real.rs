use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::{GitBackend, WindowInfo, ZmxBackend};

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

    fn fetch_prune(&self, bare_repo: &Path) -> Result<()> {
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
// ZmxBackend — wraps `zmx` (session attach/detach for the terminal)
// ---------------------------------------------------------------------------
//
// zmx has no window concept — every session is a single PTY. The head chef
// trait is window-oriented (one `yeschef` session holding many ticket windows),
// so we map each `<session>:<window>` pair onto a standalone zmx session named
// `<session>-<window>`. `session_exists`/`list_windows` then derive the
// brigade's state from the set of `<session>-…` zmx sessions.

pub struct RealZmxBackend;

/// Build the flat zmx session id for a ticket window. zmx has no windows, so each
/// window becomes its own session, namespaced under the brigade session name.
fn zid(session: &str, window: &str) -> String {
    format!("{session}-{window}")
}

/// List the names of all active zmx sessions (one per line via `zmx ls --short`).
fn zmx_sessions() -> Result<Vec<String>> {
    let output = Command::new("zmx")
        .args(["ls", "--short"])
        .output()
        .context("failed to run 'zmx ls --short'")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

impl ZmxBackend for RealZmxBackend {
    fn session_exists(&self, session: &str) -> Result<bool> {
        // The brigade "session" exists if any ticket session is registered under it.
        let prefix = format!("{session}-");
        Ok(zmx_sessions()?.iter().any(|s| s.starts_with(&prefix)))
    }

    fn ensure_session(&self, _session: &str) -> Result<()> {
        // No-op: zmx creates sessions lazily on `zmx run`, and there is no
        // parent session to hold windows — each ticket window is its own session.
        Ok(())
    }

    fn new_window(&self, session: &str, window: &str, cwd: &Path, command: &str) -> Result<()> {
        // `zmx run <name> -d <command...>` creates a detached session running
        // the command. zmx has no working-directory flag, so we `cd` first and
        // run everything through a login shell (matching the tmux behaviour).
        let id = zid(session, window);
        let full = format!(
            "cd {} && {command}",
            shell_single_quote(&cwd.to_string_lossy())
        );
        let status = Command::new("zmx")
            .args(["run", &id, "-d", "sh", "-lc", &full])
            .status()
            .context("failed to run 'zmx run'")?;
        if !status.success() {
            bail!("zmx run failed for '{id}'");
        }
        Ok(())
    }

    fn window_exists(&self, session: &str, window: &str) -> Result<bool> {
        Ok(self.list_windows(session)?.iter().any(|w| w.name == window))
    }

    fn send_keys(&self, session: &str, window: &str, text: &str) -> Result<()> {
        let id = zid(session, window);
        // `zmx send` writes raw bytes to the session PTY. Send the literal text,
        // then a carriage return as a separate event to submit it (the PTY reads
        // CR as Enter).
        let status = Command::new("zmx")
            .args(["send", &id, text])
            .status()
            .context("failed to run 'zmx send'")?;
        if !status.success() {
            bail!("zmx send failed for '{id}'");
        }
        let status = Command::new("zmx")
            .args(["send", &id, "\r"])
            .status()
            .context("failed to run 'zmx send' (Enter)")?;
        if !status.success() {
            bail!("zmx send (Enter) failed for '{id}'");
        }
        Ok(())
    }

    fn capture_pane(&self, session: &str, window: &str, lines: Option<usize>) -> Result<String> {
        let id = zid(session, window);
        // `zmx history` dumps the full session scrollback; trim to the last N
        // lines ourselves to mirror tmux's `capture-pane -S -N`.
        let output = Command::new("zmx")
            .args(["history", &id])
            .output()
            .context("failed to run 'zmx history'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("zmx history failed for '{}': {}", id, stderr.trim());
        }
        let full = String::from_utf8_lossy(&output.stdout).into_owned();
        match lines {
            Some(n) => {
                let all: Vec<&str> = full.lines().collect();
                let start = all.len().saturating_sub(n);
                let mut tail = all[start..].join("\n");
                if full.ends_with('\n') {
                    tail.push('\n');
                }
                Ok(tail)
            }
            None => Ok(full),
        }
    }

    fn list_windows(&self, session: &str) -> Result<Vec<WindowInfo>> {
        // Recover ticket windows from the set of `<session>-…` zmx sessions. zmx
        // exposes no per-session active/dead state (a finished ticket's session
        // simply disappears), so both flags are always false; a vanished ticket
        // surfaces as "gone" rather than "dead" in `status`.
        let prefix = format!("{session}-");
        let windows = zmx_sessions()?
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
        let id = zid(session, window);
        let output = Command::new("zmx")
            .args(["kill", &id, "--force"])
            .output()
            .context("failed to run 'zmx kill'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // A session that's already gone is not an error for teardown.
            if stderr.contains("not found") || stderr.contains("no such") {
                return Ok(());
            }
            bail!("zmx kill failed for '{}': {}", id, stderr.trim());
        }
        Ok(())
    }

    fn attach(&self, session: &str, window: Option<&str>) -> Result<()> {
        // zmx attaches to a single session (one PTY). With a ticket selected,
        // attach to that ticket's session directly; otherwise fall back to the
        // first live ticket session in the brigade.
        let id = if let Some(w) = window {
            zid(session, w)
        } else {
            let prefix = format!("{session}-");
            zmx_sessions()?
                .into_iter()
                .find(|s| s.starts_with(&prefix))
                .ok_or_else(|| anyhow::anyhow!("no live yeschef sessions to attach to"))?
        };
        let status = Command::new("zmx")
            .args(["attach", &id])
            .status()
            .context("failed to run 'zmx attach'")?;
        if !status.success() {
            bail!("zmx attach failed for '{id}'");
        }
        Ok(())
    }
}

/// Wrap a string in single quotes for safe inclusion in a `sh -lc` command,
/// escaping any embedded single quotes.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
