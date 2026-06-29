use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::names::{name_from_url, validate_project_name};

// ---------------------------------------------------------------------------
// project add
// ---------------------------------------------------------------------------

pub fn run_add(config: &Config, git_url: &str, name: Option<&str>) -> Result<()> {
    // Derive project name from URL if not provided
    let project_name = match name {
        Some(n) => n.to_string(),
        None => name_from_url(git_url),
    };

    validate_project_name(&project_name)?;

    // Check for duplicate
    if config.store.project_exists(&project_name)? {
        bail!(
            "project '{project_name}' already exists; choose a different name or remove the existing project"
        );
    }

    // Create project directory structure
    let bare_dir = config.bare_repo_dir(&project_name);
    let worktrees_dir = config.worktrees_dir(&project_name);

    std::fs::create_dir_all(&worktrees_dir).with_context(|| {
        format!(
            "failed to create project directory at {}",
            worktrees_dir.display()
        )
    })?;

    // Bare clone
    eprintln!("[add] cloning {} into {}...", git_url, bare_dir.display());
    config
        .git
        .clone_bare(git_url, &bare_dir)
        .with_context(|| format!("failed to clone '{git_url}'"))?;

    // Set relative worktree paths so a worktree's `.git` pointer stays valid
    // even if the project tree is moved. `git worktree add` is what actually
    // auto-writes `extensions.relativeWorktrees = true` (which some libgit2
    // consumers reject); `run_spawn` strips that after each add.
    config
        .git
        .set_config(&bare_dir, "worktree.useRelativePaths", "true")
        .context("failed to configure worktree.useRelativePaths")?;

    // Register in DB
    config
        .store
        .add_project(&project_name, git_url)
        .with_context(|| format!("failed to register project '{project_name}'"))?;

    println!("project '{project_name}' added ({git_url})");
    println!("run 'nixsand spawn {project_name} <branch>' to start an agent");
    Ok(())
}

// ---------------------------------------------------------------------------
// project list
// ---------------------------------------------------------------------------

pub fn run_list(config: &Config) -> Result<()> {
    let projects = config.store.list_projects()?;
    if projects.is_empty() {
        println!("no projects registered; run 'nixsand project add <git-url>' to add one");
    } else {
        for (name, url) in &projects {
            println!("{name}\t{url}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// refresh
// ---------------------------------------------------------------------------

/// Fetch the latest remote refs into a project's bare clone so the next
/// `spawn --base origin/<branch>` starts from the up-to-date tip. With no
/// project, refresh every registered project.
pub fn run_refresh(config: &Config, project: Option<&str>) -> Result<()> {
    if let Some(name) = project {
        if !config.store.project_exists(name)? {
            bail!("project '{name}' not found; run 'nixsand project add <git-url>' first");
        }
        refresh_one(config, name)?;
    } else {
        let projects = config.store.list_projects()?;
        if projects.is_empty() {
            println!("no projects registered; run 'nixsand project add <git-url>' to add one");
            return Ok(());
        }
        for (name, _) in &projects {
            refresh_one(config, name)?;
        }
    }
    Ok(())
}

fn refresh_one(config: &Config, name: &str) -> Result<()> {
    let bare_dir = config.bare_repo_dir(name);
    config
        .git
        .fetch_prune(&bare_dir)
        .with_context(|| format!("failed to refresh project '{name}'"))?;
    println!("refreshed '{name}'");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::{MockGitBackend, MockZmxBackend};
    use crate::store::Store;
    use tempfile::TempDir;

    /// A Config backed by mocks + an in-memory store. The returned git mock
    /// is `Arc`-backed and shares state with the copy inside `config`, so
    /// recorded calls are observable through it. Keep `_tmp` alive.
    struct Harness {
        config: Config,
        git: MockGitBackend,
        _tmp: TempDir,
    }

    fn harness() -> Harness {
        let tmp = TempDir::new().unwrap();
        let store = Store::open_in_memory().unwrap();
        let git = MockGitBackend::new();
        let config = Config {
            home: tmp.path().to_path_buf(),
            store,
            git: Box::new(git.clone()),
            zmx: Box::new(MockZmxBackend::new()),
        };
        Harness { config, git, _tmp: tmp }
    }

    #[test]
    fn refresh_fetches_the_named_project() {
        let h = harness();
        h.config
            .store
            .add_project("proj", "https://example.com/proj.git")
            .unwrap();
        run_refresh(&h.config, Some("proj")).unwrap();
        let bare = h.config.bare_repo_dir("proj");
        assert!(
            h.git
                .recorded_calls()
                .contains(&format!("fetch_prune:{}", bare.display())),
            "calls: {:?}",
            h.git.recorded_calls()
        );
    }

    #[test]
    fn refresh_unknown_project_errors() {
        let h = harness();
        let err = run_refresh(&h.config, Some("nope")).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn refresh_all_fetches_every_project() {
        let h = harness();
        h.config
            .store
            .add_project("a", "https://example.com/a.git")
            .unwrap();
        h.config
            .store
            .add_project("b", "https://example.com/b.git")
            .unwrap();
        run_refresh(&h.config, None).unwrap();
        let calls = h.git.recorded_calls();
        let fetches = calls.iter().filter(|c| c.starts_with("fetch_prune:")).count();
        assert_eq!(fetches, 2, "calls: {calls:?}");
    }
}
