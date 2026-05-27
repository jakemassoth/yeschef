use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::backend::real::{check_binary, RealContainerBackend, RealGitBackend, RealZmxBackend};
use crate::backend::{ContainerBackend, GitBackend, ZmxBackend};
use crate::store::Store;

/// Runtime configuration: home directory + backend handles.
pub struct Config {
    pub home: PathBuf,
    pub store: Store,
    pub container: Box<dyn ContainerBackend>,
    pub git: Box<dyn GitBackend>,
    pub zmx: Box<dyn ZmxBackend>,
}

impl Config {
    /// Build the runtime config from environment / defaults.
    /// This validates the platform and opens the store.
    pub fn load() -> Result<Self> {
        let home = resolve_home()?;
        let db_path = home.join("nixsand.db");
        let store = Store::open(&db_path).context("failed to open nixsand database")?;
        Ok(Self {
            home,
            store,
            container: Box::new(RealContainerBackend),
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

/// Validate that we're running on macOS aarch64.
pub fn check_platform() -> Result<()> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    if os != "macos" || arch != "aarch64" {
        bail!(
            "nixsand requires macOS on Apple Silicon (aarch64), but detected {os} {arch}"
        );
    }
    Ok(())
}

/// Validate that all required host binaries are available.
pub fn check_host_deps() -> Result<()> {
    check_binary("container").context("'container' (Apple's container CLI) is required")?;
    check_binary("tmux").context("'tmux' is required")?;
    check_binary("git").context("'git' is required")?;
    Ok(())
}
