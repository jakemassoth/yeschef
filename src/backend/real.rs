use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::{BranchStatus, GitBackend, HerdrBackend, Workspace, WorkspaceInfo};

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
// HerdrBackend — wraps the `herdr` agent multiplexer
// ---------------------------------------------------------------------------
//
// The whole brigade lives in ONE named herdr session (`resolve_herdr_session`,
// default `yeschef`), served by a single background herdr server. Each line cook
// is a herdr WORKSPACE labelled `<project>/<branch>`, whose root PANE runs the
// agent; the head chef is another workspace. Naming the session is what isolates
// yeschef's brigade from a human's own default `herdr` session — the analog of
// the old tmux backend's private `-L` socket.
//
// yeschef drives everything by shelling out to the `herdr` CLI (arm's-length,
// exactly as the tmux backend shelled out to `tmux`), never linking herdr as a
// library. herdr's socket-API subcommands emit JSON, from which we parse the
// opaque workspace/pane ids and the live agent status.
//
// The session name is configurable via `YESCHEF_HERDR_SESSION` (default
// `yeschef`), resolved once per backend in `new`. Production leaves it unset and
// runs on `yeschef`; the e2e tests point it at a throwaway per-test session (with
// a short `XDG_CONFIG_HOME`, since herdr derives its socket path from there) so
// they can create and tear down sessions without ever touching the operator's
// live `yeschef` brigade.

/// Default herdr session name for the brigade, used when
/// [`HERDR_SESSION_ENV`] is unset. A named session isolates yeschef's
/// workspaces from a human's own default `herdr` session.
pub const DEFAULT_HERDR_SESSION: &str = "yeschef";

/// The env var that overrides the herdr session name.
pub const HERDR_SESSION_ENV: &str = "YESCHEF_HERDR_SESSION";

/// The `--source` id yeschef stamps on the display metadata it reports, so its
/// decoration is distinct from any other reporter and from herdr's own
/// live-detection lifecycle authority.
const REPORT_SOURCE: &str = "yeschef";

/// How long `ensure_server` waits for a freshly-spawned server to accept
/// connections before giving up.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve the herdr session name from [`HERDR_SESSION_ENV`], falling back to
/// [`DEFAULT_HERDR_SESSION`] when it is unset or empty. This is the single
/// source of truth for which herdr session every yeschef invocation drives —
/// making it configurable is what lets the e2e suite run on a throwaway per-test
/// session instead of the operator's live `yeschef` brigade.
pub fn resolve_herdr_session() -> String {
    match std::env::var(HERDR_SESSION_ENV) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => DEFAULT_HERDR_SESSION.to_string(),
    }
}

pub struct RealHerdrBackend {
    /// The herdr session name this backend drives, resolved once from
    /// [`resolve_herdr_session`] at construction and passed as `--session` on
    /// every invocation.
    session: String,
}

impl RealHerdrBackend {
    pub fn new() -> Self {
        Self {
            session: resolve_herdr_session(),
        }
    }

    /// A `herdr` command pre-wired to the brigade session (`--session <name>`).
    /// The global `--session` flag must precede the subcommand, so callers append
    /// the subcommand and its args after this.
    fn cmd(&self) -> Command {
        let mut c = Command::new("herdr");
        c.arg("--session").arg(&self.session);
        c
    }

    /// Run a herdr subcommand and return its stdout, erroring with the command's
    /// stderr on failure.
    fn run_json(&self, args: &[&str]) -> Result<String> {
        let output = self
            .cmd()
            .args(args)
            .output()
            .with_context(|| format!("failed to run 'herdr {}'", args.join(" ")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("herdr {} failed: {}", args.join(" "), stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

// --- JSON shapes emitted by herdr's socket-API subcommands -----------------
//
// Only the fields yeschef needs are declared; `serde` ignores the rest.

/// `herdr workspace create` → `{ "result": { "workspace": {...}, "root_pane": {...} } }`
#[derive(Deserialize)]
struct CreateEnvelope {
    result: CreateResult,
}

#[derive(Deserialize)]
struct CreateResult {
    workspace: WorkspaceObj,
    root_pane: PaneObj,
}

/// `herdr workspace list` → `{ "result": { "workspaces": [ {...}, ... ] } }`
#[derive(Deserialize)]
struct ListEnvelope {
    result: ListResult,
}

#[derive(Deserialize)]
struct ListResult {
    workspaces: Vec<WorkspaceObj>,
}

#[derive(Deserialize)]
struct WorkspaceObj {
    workspace_id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    agent_status: String,
}

#[derive(Deserialize)]
struct PaneObj {
    pane_id: String,
}

impl HerdrBackend for RealHerdrBackend {
    fn server_running(&self) -> Result<bool> {
        // `status server` reports the running server for this session. When the
        // server is down it exits non-zero (or prints a non-running status);
        // either way that is "not running", not an error.
        let output = self
            .cmd()
            .args(["status", "server"])
            .output()
            .context("failed to run 'herdr status server'")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(output.status.success() && stdout.contains("status: running"))
    }

    fn ensure_server(&self) -> Result<()> {
        // Idempotent: a running brigade server is left untouched.
        if self.server_running()? {
            return Ok(());
        }
        // Launch the headless server detached: a new session (`setsid`) with no
        // controlling terminal and its stdio discarded, so it outlives this
        // one-shot yeschef process (and the agents running inside it survive
        // after `spawn` returns) and is immune to the invoking terminal's SIGHUP.
        let mut cmd = self.cmd();
        cmd.arg("server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: `setsid` is async-signal-safe and touches no shared state in
        // the forked child before the exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().context("failed to start the herdr server")?;

        // Wait for the freshly-spawned server to accept connections.
        let deadline = Instant::now() + SERVER_START_TIMEOUT;
        while Instant::now() < deadline {
            if self.server_running()? {
                return Ok(());
            }
            sleep(Duration::from_millis(50));
        }
        bail!("herdr server did not come up within {SERVER_START_TIMEOUT:?}");
    }

    fn stop_server(&self) -> Result<()> {
        // A no-op if the server is already down.
        if !self.server_running()? {
            return Ok(());
        }
        let output = self
            .cmd()
            .args(["server", "stop"])
            .output()
            .context("failed to run 'herdr server stop'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("herdr server stop failed: {}", stderr.trim());
        }
        Ok(())
    }

    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<Workspace> {
        let cwd = cwd.to_string_lossy();
        let stdout = self.run_json(&[
            "workspace",
            "create",
            "--cwd",
            &cwd,
            "--label",
            label,
            "--no-focus",
        ])?;
        let env: CreateEnvelope = serde_json::from_str(&stdout).with_context(|| {
            format!("failed to parse 'herdr workspace create' output: {stdout}")
        })?;
        Ok(Workspace {
            workspace_id: env.result.workspace.workspace_id,
            pane_id: env.result.root_pane.pane_id,
        })
    }

    fn run_in_pane(&self, pane_id: &str, text: &str) -> Result<()> {
        // `pane run` types the text into the pane and submits it (Enter),
        // mirroring the old tmux `send-keys -l` + `Enter`.
        let output = self
            .cmd()
            .args(["pane", "run", pane_id, text])
            .output()
            .context("failed to run 'herdr pane run'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("herdr pane run failed for '{pane_id}': {}", stderr.trim());
        }
        Ok(())
    }

    fn read_pane(&self, pane_id: &str, lines: Option<usize>) -> Result<String> {
        // `pane read --source recent` returns the pane's recent output as plain
        // (de-styled) text; `--lines N` limits it to the last N lines.
        let mut cmd = self.cmd();
        cmd.args(["pane", "read", pane_id, "--source", "recent"]);
        let lines_str;
        if let Some(n) = lines {
            lines_str = n.to_string();
            cmd.args(["--lines", &lines_str]);
        }
        let output = cmd.output().context("failed to run 'herdr pane read'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("herdr pane read failed for '{pane_id}': {}", stderr.trim());
        }
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        Ok(text)
    }

    fn set_display_status(&self, pane_id: &str, status: &str) -> Result<()> {
        // Display-only pane metadata (a `status=<STATUS>` token), so herdr's TUI
        // can surface the cook's self-reported task status. This does NOT touch
        // herdr's own live agent-status detection.
        let token = format!("status={status}");
        let output = self
            .cmd()
            .args([
                "pane",
                "report-metadata",
                pane_id,
                "--source",
                REPORT_SOURCE,
                "--token",
                &token,
            ])
            .output()
            .context("failed to run 'herdr pane report-metadata'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "herdr pane report-metadata failed for '{pane_id}': {}",
                stderr.trim()
            );
        }
        Ok(())
    }

    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        // If the server isn't up there are no workspaces — an empty brigade,
        // not an error (mirrors the old backend's tolerance of a missing session).
        if !self.server_running()? {
            return Ok(Vec::new());
        }
        let stdout = self.run_json(&["workspace", "list"])?;
        let env: ListEnvelope = serde_json::from_str(&stdout)
            .with_context(|| format!("failed to parse 'herdr workspace list' output: {stdout}"))?;
        Ok(env
            .result
            .workspaces
            .into_iter()
            .map(|w| WorkspaceInfo {
                workspace_id: w.workspace_id,
                label: w.label,
                agent_status: w.agent_status,
            })
            .collect())
    }

    fn close_workspace(&self, workspace_id: &str) -> Result<()> {
        let output = self
            .cmd()
            .args(["workspace", "close", workspace_id])
            .output()
            .context("failed to run 'herdr workspace close'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // A workspace that's already gone is not an error for teardown.
            let s = stderr.to_lowercase();
            if s.contains("not found") || s.contains("no such") || s.contains("unknown") {
                return Ok(());
            }
            bail!(
                "herdr workspace close failed for '{workspace_id}': {}",
                stderr.trim()
            );
        }
        Ok(())
    }

    fn attach(&self, workspace_id: Option<&str>) -> Result<()> {
        // Focus the requested workspace first (best-effort) so the TUI lands
        // there, then hand the terminal to herdr's native UI. Bare
        // `herdr --session <name>` attaches/launches the TUI, inheriting stdio.
        if let Some(ws) = workspace_id {
            let _ = self.cmd().args(["workspace", "focus", ws]).status();
        }
        let status = self
            .cmd()
            .status()
            .context("failed to run 'herdr' (attach TUI)")?;
        if !status.success() {
            bail!("herdr attach exited with failure");
        }
        Ok(())
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
