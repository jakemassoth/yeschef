use std::path::Path;
use anyhow::Result;

pub mod real;
pub mod mock;

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
    fn default_branch(&self, bare_repo: &Path) -> Result<String>;
}

/// Liveness/identity info for a single task window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub name: String,
    pub active: bool,
    pub dead: bool,
}

/// Trait abstracting `zmx` terminal session operations.
///
/// The orchestrator models tasks as windows under a single `nixsand` session
/// (see `names::nixsand_session`). zmx has no windows — the real backend maps
/// each `<session>:<window>` onto a standalone zmx session. The orchestrator
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
    fn list_windows(&self, session: &str) -> Result<Vec<WindowInfo>>;
    fn kill_window(&self, session: &str, window: &str) -> Result<()>;
    /// Attach to the session; if `window` is given, select it first.
    fn attach(&self, session: &str, window: Option<&str>) -> Result<()>;
}
