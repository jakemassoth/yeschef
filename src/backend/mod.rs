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

/// Liveness/identity info for a single ticket window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub name: String,
    pub active: bool,
    pub dead: bool,
}

/// Trait abstracting `tmux` terminal session operations.
///
/// The whole brigade lives in **one** tmux session (`names::yeschef_session`):
/// the pinned head chef at window 0 (`names::headchef_window`) and one window
/// per line cook, each named `<project>-<branch>`. A ticket "window" therefore
/// maps onto a real tmux window addressed as `<session>:<window>` (see
/// `backend::real`) — which is exactly what lets `tmux attach` show every cook
/// together in a native tab bar (the yeschef TUI). The session runs on a private
/// tmux server (a dedicated `-L` socket) loaded with yeschef's own config, so it
/// never touches the user's tmux server or `~/.tmux.conf`. The head chef drives
/// windows via `send_keys`/`capture_pane` without being attached; the human
/// attaches to watch and to talk to the head chef.
pub trait TmuxBackend: Send + Sync {
    /// Whether the brigade session itself exists.
    fn session_exists(&self, session: &str) -> Result<bool>;
    /// Ensure the brigade session exists. If absent, create it (detached) with
    /// window 0 named `head_window`, running `head_command` in `head_cwd` — the
    /// pinned head chef. Idempotent: an existing session is left untouched
    /// (the head chef is never restarted or duplicated).
    fn ensure_session(
        &self,
        session: &str,
        head_window: &str,
        head_cwd: &Path,
        head_command: &str,
    ) -> Result<()>;
    /// Create a new window in the session running `command` in `cwd`. The
    /// session must already exist (see `ensure_session`).
    fn new_window(&self, session: &str, window: &str, cwd: &Path, command: &str) -> Result<()>;
    /// Restart the process in an existing window *in place*: kill whatever is
    /// running in its pane and relaunch `command` in `cwd`, keeping the window
    /// itself — its name, tab position, and `@status` decoration — intact. This
    /// is what `restart` uses to swap a running agent for a fresh process (e.g.
    /// to pick up a Claude Code update) without disturbing the brigade layout;
    /// unlike kill + `new_window`, the tab never disappears and reappears. The
    /// window must already exist.
    fn respawn_window(&self, session: &str, window: &str, cwd: &Path, command: &str) -> Result<()>;
    fn window_exists(&self, session: &str, window: &str) -> Result<bool>;
    /// Send a single line of text followed by Enter into a window.
    fn send_keys(&self, session: &str, window: &str, text: &str) -> Result<()>;
    /// Capture the visible pane of a window. `lines` limits to the last N lines.
    fn capture_pane(&self, session: &str, window: &str, lines: Option<usize>) -> Result<String>;
    /// Set the per-window `@status` user option that yeschef's `tmux.conf`
    /// renders as a colour-coded tab in the status line. Called on every
    /// `ticket ... status-set` so the brigade tab bar reflects a cook's
    /// self-reported status live, with no polling and no rendering on our side.
    fn set_window_status(&self, session: &str, window: &str, status: &str) -> Result<()>;
    /// List every window in the session (head chef included). Callers match the
    /// names against the ticket registry to build the brigade view.
    fn list_windows(&self, session: &str) -> Result<Vec<WindowInfo>>;
    fn kill_window(&self, session: &str, window: &str) -> Result<()>;
    /// Attach to the session; if `window` is given, select it on attach.
    fn attach(&self, session: &str, window: Option<&str>) -> Result<()>;
}
