use anyhow::Result;
use std::path::Path;

pub mod mock;
pub mod real;

/// How a ticket branch relates to the project's main line — drives whether
/// `cleanup` may safely reap it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchStatus {
    /// Tip is an ancestor of the main line: the work is in `main`. Safe to reap.
    Merged,
    /// Upstream tracking branch was deleted on the remote (detected via
    /// `fetch --prune`): the branch landed and was cleaned up. Safe to reap.
    Gone,
    /// Has commits not on the main line and a live (or unset) upstream: still
    /// active / unmerged work. Keep it.
    Unmerged,
}

/// Trait abstracting git operations.
pub trait GitBackend: Send + Sync {
    fn clone_bare(&self, url: &str, dest: &Path) -> Result<()>;
    fn set_config(&self, repo: &Path, key: &str, value: &str) -> Result<()>;
    /// Remove a config key. Succeeds if the key is already absent.
    fn unset_config(&self, repo: &Path, key: &str) -> Result<()>;
    fn add_worktree(
        &self,
        bare_repo: &Path,
        worktree_path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<()>;
    /// Remove a worktree registration (and prune stale metadata).
    fn remove_worktree(&self, bare_repo: &Path, worktree_path: &Path) -> Result<()>;
    /// Delete a local branch from the repo (force). Succeeds if already absent.
    fn delete_branch(&self, bare_repo: &Path, branch: &str) -> Result<()>;
    fn default_branch(&self, bare_repo: &Path) -> Result<String>;
    /// Configure `origin` to fetch into remote-tracking refs
    /// (`+refs/heads/*:refs/remotes/origin/*`). A plain `git clone --bare`
    /// leaves no fetch refspec, so `origin/<branch>` never resolves; setting
    /// this and fetching makes `origin/main` available. Idempotent — safe to
    /// call repeatedly to repair clones created before this was set.
    fn ensure_tracking_refspec(&self, bare_repo: &Path) -> Result<()>;
    /// Fetch the latest refs from `origin` into the bare clone, pruning
    /// deleted remote branches.
    fn fetch_prune(&self, bare_repo: &Path) -> Result<()>;
    /// Classify a ticket branch relative to the project's main line
    /// (`main_ref`, e.g. `origin/main`) so `cleanup` can decide whether to
    /// reap it. Must not mutate the repo. Run `fetch_prune` first so the
    /// merged / gone determination reflects the latest remote state.
    fn branch_status(&self, bare_repo: &Path, branch: &str, main_ref: &str)
        -> Result<BranchStatus>;
}

/// A herdr workspace yeschef has created for a ticket: its workspace id and the
/// id of its root pane (where the agent runs). Both are opaque ids minted by
/// herdr and parsed out of its JSON — yeschef persists them in the store and
/// addresses the ticket by them thereafter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub workspace_id: String,
    pub pane_id: String,
}

/// Liveness/identity for one herdr workspace, from `herdr workspace list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    /// The workspace label yeschef set at creation — `<project>/<branch>` for a
    /// cook, or the head-chef label. Human-facing; yeschef matches tickets by
    /// `workspace_id`, not by label.
    pub label: String,
    /// herdr's live-detected aggregate agent status for the workspace
    /// (`idle`/`working`/`blocked`/`done`/`unknown`).
    pub agent_status: String,
}

/// Trait abstracting the [`herdr`](https://github.com/ogulcancelik/herdr) agent
/// multiplexer, which yeschef drives as an external CLI (arm's-length, exactly
/// as the old tmux backend shelled out to `tmux`).
///
/// The whole brigade lives in **one** named herdr session (see
/// `backend::real::resolve_herdr_session`, default `yeschef`), served by a
/// single background herdr server. Each line cook is a herdr **workspace**
/// labelled `<project>/<branch>`, whose root **pane** runs the agent; the head
/// chef is another workspace. Naming the session is what isolates yeschef's
/// brigade from a human's own default `herdr` session. The head chef drives cook
/// panes via `run_in_pane`/`read_pane` over the socket API without being
/// attached; `attach` hands the terminal to herdr's native TUI (the yeschef
/// TUI), where every workspace shows as a live, status-coloured entry.
pub trait HerdrBackend: Send + Sync {
    /// Ensure the brigade's herdr server is running, starting it (detached,
    /// headless) if not. Idempotent — an already-running server is left alone.
    fn ensure_server(&self) -> Result<()>;
    /// Whether the brigade's herdr server is currently running.
    fn server_running(&self) -> Result<bool>;
    /// Stop the brigade's herdr server. herdr persists the session shape to
    /// disk, so a subsequent `ensure_server` restores the workspaces (and, with
    /// `resume_agents_on_restore`, resumes each agent's conversation) — this is
    /// what `restart` builds on. A no-op if the server is already down.
    fn stop_server(&self) -> Result<()>;
    /// Create a workspace labelled `label`, its root pane rooted at `cwd`, and
    /// return the workspace + root-pane ids. The server must already be running
    /// (see `ensure_server`). The pane starts an interactive shell; launch the
    /// agent into it with `run_in_pane`.
    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<Workspace>;
    /// Type a line of text into a pane and submit it (Enter). Used both to
    /// launch the agent into a fresh pane and to steer a running one.
    fn run_in_pane(&self, pane_id: &str, text: &str) -> Result<()>;
    /// Read a pane's recent terminal output. `lines` limits to the last N lines.
    fn read_pane(&self, pane_id: &str, lines: Option<usize>) -> Result<String>;
    /// Attach display-only metadata to a pane recording the cook's self-reported
    /// task status, so herdr's TUI can surface it. Best-effort decoration — it
    /// does not touch herdr's own live agent-status detection.
    fn set_display_status(&self, pane_id: &str, status: &str) -> Result<()>;
    /// List every workspace in the brigade session (head chef included). Callers
    /// match `workspace_id` against the ticket registry to build the brigade
    /// view and to tell live tickets from gone ones.
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>>;
    /// Close a workspace (and everything in it). A workspace that is already gone
    /// is not an error — teardown must be idempotent.
    fn close_workspace(&self, workspace_id: &str) -> Result<()>;
    /// Hand the terminal to herdr's native TUI for the brigade session. If a
    /// `workspace_id` is given, focus it first so attach lands there.
    fn attach(&self, workspace_id: Option<&str>) -> Result<()>;
}
