use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::backend::real::{check_binary, RealGitBackend, RealZmxBackend};
use crate::backend::{GitBackend, ZmxBackend};
use crate::store::Store;

/// Runtime configuration: home directory + backend handles.
pub struct Config {
    pub home: PathBuf,
    pub store: Store,
    pub git: Box<dyn GitBackend>,
    pub zmx: Box<dyn ZmxBackend>,
}

impl Config {
    /// Build the runtime config from environment / defaults.
    /// This opens the store and wires up the real backends.
    pub fn load() -> Result<Self> {
        let home = resolve_home()?;
        let db_path = home.join("nixsand.db");
        let store = Store::open(&db_path).context("failed to open nixsand database")?;
        Ok(Self {
            home,
            store,
            git: Box::new(RealGitBackend),
            zmx: Box::new(RealZmxBackend),
        })
    }

    /// Build a config without platform/binary checks (used by init).
    #[allow(dead_code)]
    pub fn load_unchecked() -> Result<(PathBuf, Store)> {
        let home = resolve_home()?;
        // Ensure home dir exists before opening the DB
        std::fs::create_dir_all(&home)
            .with_context(|| format!("failed to create nixsand home at {}", home.display()))?;
        let db_path = home.join("nixsand.db");
        let store = Store::open(&db_path).context("failed to open nixsand database")?;
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

    /// Directory holding per-task spawn prompt files. Lives under the nixsand
    /// home (outside any project worktree) so prompts can't be committed.
    pub fn prompts_dir(&self) -> PathBuf {
        self.home.join("prompts")
    }
}

/// Resolve the nixsand home directory.
/// Uses `NIXSAND_HOME` env var if set, otherwise ~/.nixsand.
pub fn resolve_home() -> Result<PathBuf> {
    if let Ok(env_home) = std::env::var("NIXSAND_HOME") {
        return Ok(PathBuf::from(env_home));
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".nixsand"))
}

/// Validate that all required host binaries are available.
pub fn check_host_deps() -> Result<()> {
    check_binary("git").context("'git' is required")?;
    check_binary("zmx").context("'zmx' is required")?;
    Ok(())
}
