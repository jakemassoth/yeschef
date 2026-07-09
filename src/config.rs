use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::backend::real::{check_binary, RealGitBackend, RealTmuxBackend};
use crate::backend::{GitBackend, TmuxBackend};
use crate::store::Store;

/// yeschef's own tmux configuration, baked into the binary and written to
/// `<home>/tmux.conf` at load. Loaded via `tmux -f` on a private socket so
/// yeschef never reads or clobbers the user's `~/.tmux.conf`. Ships the
/// `extended-keys` settings that let Claude Code see Shift+Enter.
const TMUX_CONF: &str = include_str!("../tmux.conf");

/// Runtime configuration: home directory + backend handles.
pub struct Config {
    pub home: PathBuf,
    pub store: Store,
    pub git: Box<dyn GitBackend>,
    pub tmux: Box<dyn TmuxBackend>,
}

impl Config {
    /// Build the runtime config from environment / defaults.
    /// This opens the store and wires up the real backends.
    pub fn load() -> Result<Self> {
        let home = resolve_home()?;
        let db_path = home.join("yeschef.db");
        let store = Store::open(&db_path).context("failed to open yeschef database")?;
        let tmux_conf = ensure_tmux_conf(&home)?;
        Ok(Self {
            home,
            store,
            git: Box::new(RealGitBackend),
            tmux: Box::new(RealTmuxBackend::new(tmux_conf)),
        })
    }

    /// Build a config without platform/binary checks (used by init).
    #[allow(dead_code)]
    pub fn load_unchecked() -> Result<(PathBuf, Store)> {
        let home = resolve_home()?;
        // Ensure home dir exists before opening the DB
        std::fs::create_dir_all(&home)
            .with_context(|| format!("failed to create yeschef home at {}", home.display()))?;
        let db_path = home.join("yeschef.db");
        let store = Store::open(&db_path).context("failed to open yeschef database")?;
        Ok((home, store))
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.home.join("projects")
    }

    pub fn project_dir(&self, project: &str) -> PathBuf {
        self.projects_dir().join(project)
    }

    pub fn bare_repo_dir(&self, project: &str) -> PathBuf {
        self.project_dir(project).join(".bare")
    }

    pub fn worktrees_dir(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("worktrees")
    }

    pub fn worktree_dir(&self, project: &str, branch: &str) -> PathBuf {
        self.worktrees_dir(project).join(branch)
    }

    /// Directory holding per-ticket spawn prompt files. Lives under the yeschef
    /// home (outside any project worktree) so prompts can't be committed.
    pub fn prompts_dir(&self) -> PathBuf {
        self.home.join("prompts")
    }
}

/// Resolve the yeschef home directory.
/// Uses `YESCHEF_HOME` env var if set, otherwise ~/yeschef.
pub fn resolve_home() -> Result<PathBuf> {
    if let Ok(env_home) = std::env::var("YESCHEF_HOME") {
        return Ok(PathBuf::from(env_home));
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join("yeschef"))
}

/// Resolve the yeschef **source checkout** directory — the working directory
/// for the head-chef Claude Code session the TUI pins (see `commands::tui`).
/// Defaults to the canonical `~/yeschef/yeschef-src` (see CLAUDE.md), and is
/// overridable via `YESCHEF_SRC` for non-standard layouts and tests.
///
/// Deliberately *not* `env!("CARGO_MANIFEST_DIR")`: the shipped binary is
/// commonly built via nix, so the compile-time manifest path points into the
/// read-only nix store rather than the user's editable checkout. This resolves
/// the canonical runtime path instead, independent of where the binary lives.
pub fn resolve_src_dir() -> Result<PathBuf> {
    if let Ok(env_src) = std::env::var("YESCHEF_SRC") {
        return Ok(PathBuf::from(env_src));
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join("yeschef").join("yeschef-src"))
}

/// Write yeschef's own tmux config to `<home>/tmux.conf` and return its path.
/// Rewritten on every load so config changes ship with the binary. The private
/// tmux server is launched with `tmux -f <this path>` (see `backend::real`),
/// isolating yeschef's sessions from the user's `~/.tmux.conf`.
pub fn ensure_tmux_conf(home: &std::path::Path) -> Result<PathBuf> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("failed to create yeschef home at {}", home.display()))?;
    let path = home.join("tmux.conf");
    std::fs::write(&path, TMUX_CONF)
        .with_context(|| format!("failed to write tmux config at {}", path.display()))?;
    Ok(path)
}

/// Validate that all required host binaries are available.
pub fn check_host_deps() -> Result<()> {
    check_binary("git").context("'git' is required")?;
    check_binary("tmux").context("'tmux' is required")?;
    Ok(())
}
