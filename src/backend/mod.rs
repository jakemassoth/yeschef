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

/// Trait abstracting `zmx` terminal session operations.
///
/// The head chef models tickets as windows under a single `yeschef` session
/// (see `names::yeschef_session`). zmx has no windows — the real backend maps
/// each `<session>:<window>` onto a standalone zmx session. The head chef
/// drives windows via `send_keys`/`capture_pane` without being attached; the
/// human attaches separately to watch.
pub trait ZmxBackend: Send + Sync {
    fn session_exists(&self, session: &str) -> Result<bool>;
    /// Create the session (detached) if it does not already exist.
    fn ensure_session(&self, session: &str) -> Result<()>;
    /// Create a new window running `command` with working directory `cwd`.
    fn new_window(&self, session: &str, window: &str, cwd: &Path, command: &str) -> Result<()>;
    fn window_exists(&self, session: &str, window: &str) -> Result<bool>;
    /// Send a single line of text followed by Enter into a window.
    fn send_keys(&self, session: &str, window: &str, text: &str) -> Result<()>;
    /// Capture the visible pane of a window. `lines` limits to the last N lines.
    fn capture_pane(&self, session: &str, window: &str, lines: Option<usize>) -> Result<String>;
    /// Capture a window's full scrollback as a VT/ANSI byte stream (colours,
    /// attributes, cursor-addressed redraws) rather than de-styled text —
    /// suitable for replaying through a real terminal-emulation parser.
    /// Unlike [`capture_pane`](Self::capture_pane) this must not be trimmed
    /// by naive line-splitting: an escape sequence straddling the trim point
    /// would be truncated mid-sequence and corrupt the replay.
    fn capture_pane_styled(&self, session: &str, window: &str) -> Result<String>;
    fn list_windows(&self, session: &str) -> Result<Vec<WindowInfo>>;
    fn kill_window(&self, session: &str, window: &str) -> Result<()>;
    /// Attach to the session; if `window` is given, select it first.
    fn attach(&self, session: &str, window: Option<&str>) -> Result<()>;

    // ---- Bare-session (raw id) operations --------------------------------
    //
    // The methods above address a ticket window and go through the brigade's
    // `<session>-<window>` id mapping. The `*_raw` methods below target a
    // standalone zmx session by its exact id, with no namespacing — used for
    // the TUI's pinned head-chef session (`names::headchef_session`), which is
    // a bare `headchef` session running Claude Code rather than a brigade
    // ticket. See `commands::tui`.

    /// Ensure a bare zmx session with the exact id `id` exists, launching
    /// `command` in `cwd` if it is absent. Idempotent: an already-running
    /// session is left untouched (never restarted or duplicated).
    fn ensure_raw_session(&self, id: &str, cwd: &Path, command: &str) -> Result<()>;
    /// Capture a bare session's full scrollback as a VT/ANSI byte stream, like
    /// [`capture_pane_styled`](Self::capture_pane_styled) but addressing the
    /// session by its raw id. Must not be trimmed by naive line-splitting for
    /// the same reason (a straddling escape sequence would be corrupted).
    fn capture_raw_styled(&self, id: &str) -> Result<String>;
    /// Attach to a bare session by its raw id, like [`attach`](Self::attach)
    /// but without the brigade-window namespacing.
    fn attach_raw(&self, id: &str) -> Result<()>;
}
