use anyhow::{anyhow, bail, Context, Result};

use crate::backend::Mount;
use crate::config::Config;
use crate::guard::RollbackGuard;
use crate::image::{ensure_base_image, ensure_project_image};
use crate::names::{
    container_name, name_from_url, project_image_tag, sanitize_branch, validate_project_name,
    zmx_session_name,
};

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
    let _project_dir = config.project_dir(&project_name);
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

    // Set relative worktree paths so the worktree's .git pointer resolves
    // both on the host and inside the container that bind-mounts the project.
    // Note: `git worktree add` is what actually auto-writes
    // `extensions.relativeWorktrees = true` (which breaks libgit2/nix); we
    // strip that in `run_branch` after each worktree add.
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
// project branch
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
pub fn run_branch(
    config: &Config,
    project: &str,
    branch: &str,
    base: Option<&str>,
) -> Result<()> {
    // Validate project exists
    if !config.store.project_exists(project)? {
        bail!(
            "project '{project}' not found; run 'nixsand project add <git-url>' first"
        );
    }

    let sanitized = sanitize_branch(branch);
    let bare_dir = config.bare_repo_dir(project);
    let worktree_path = config.worktree_dir(project, branch);
    let container_nm = container_name(project, &sanitized);

    // Determine base
    let base_branch = match base {
        Some(b) => b.to_string(),
        None => config
            .git
            .default_branch(&bare_dir)
            .context("failed to determine default branch")?,
    };

    eprintln!(
        "[branch] provisioning branch '{branch}' from '{base_branch}' for project '{project}'"
    );

    // Rollback guard
    let mut guard = RollbackGuard::new();

    // 1. Create worktree (if it doesn't already exist)
    if worktree_path.exists() {
        eprintln!("[branch] worktree already exists at {}", worktree_path.display());
    } else {
        config
            .git
            .add_worktree(&bare_dir, &worktree_path, branch, &base_branch)
            .with_context(|| {
                format!(
                    "failed to create worktree for branch '{}' at {}",
                    branch,
                    worktree_path.display()
                )
            })?;

        let wt = worktree_path.clone();
        guard.push(move || {
            eprintln!("[rollback] removing worktree at {}", wt.display());
            if let Err(e) = std::fs::remove_dir_all(&wt) {
                eprintln!("[rollback] failed to remove worktree: {e}");
            }
        });
    }

    // `git worktree add` with useRelativePaths enabled bumps the bare repo's
    // repositoryformatversion to 1 and writes `extensions.relativeWorktrees = true`.
    // libgit2 (used by nix) rejects unknown extension names and refuses to open
    // the worktree, breaking `nix develop` inside the sandbox. The relative
    // gitdir paths still resolve correctly without the extension marker, so we
    // strip it after each worktree add.
    config
        .git
        .unset_config(&bare_dir, "extensions.relativeWorktrees")
        .context("failed to unset extensions.relativeWorktrees on bare repo")?;

    // 2. Read flake files from git (for image decisions)
    let flake_nix = config
        .git
        .read_file(&bare_dir, "flake.nix")
        .unwrap_or_default();
    let flake_lock = config
        .git
        .read_file(&bare_dir, "flake.lock")
        .unwrap_or_default();

    // 3. Ensure base image
    ensure_base_image(config.container.as_ref()).context("failed to ensure base image")?;

    // 4. Ensure per-project image
    ensure_project_image(
        project,
        config.container.as_ref(),
        &config.store,
        &flake_nix,
        &flake_lock,
    )
    .context("failed to ensure per-project image")?;

    // 5. Create and start container (if not already running)
    let project_image = project_image_tag(project);
    let project_dir = config.project_dir(project);
    let claude_dir = dirs::home_dir().map_or_else(|| std::path::PathBuf::from("/root/.claude"), |h| h.join(".claude"));

    if config.container.container_exists(&container_nm)? {
        eprintln!("[branch] container '{container_nm}' already exists");
    } else {
        let mut mounts = vec![Mount {
            host_path: project_dir.to_string_lossy().to_string(),
            container_path: "/workspace".to_string(),
        }];
        if claude_dir.exists() {
            // Mounted at sandbox's home: the container exec drops to the
            // `sandbox` user before launching claude (claude refuses to run
            // with `--dangerously-skip-permissions` under uid 0).
            mounts.push(Mount {
                host_path: claude_dir.to_string_lossy().to_string(),
                container_path: "/home/sandbox/.claude".to_string(),
            });
        }

        // Entrypoint script (baked into the image) forks `nix-daemon` before
        // executing the args, so `nix develop` from the sandbox user can reach
        // the daemon socket at `/nix/var/nix/daemon-socket/socket`. Without
        // this, sandbox has no way to mutate the nix store.
        config
            .container
            .create_container(
                &container_nm,
                &project_image,
                &mounts,
                &["/usr/local/bin/nixsand-init", "sleep", "infinity"],
            )
            .with_context(|| format!("failed to create container '{container_nm}'"))?;

        let cn = container_nm.clone();
        // Note: the guard closure can't hold a reference to config.container, so
        // we record the container name and emit a warning on rollback.
        guard.push(move || {
            eprintln!("[rollback] note: container '{cn}' may need manual removal");
        });
    }

    if config.container.container_running(&container_nm)? {
        eprintln!("[branch] container '{container_nm}' is already running");
    } else {
        config
            .container
            .start_container(&container_nm)
            .with_context(|| format!("failed to start container '{container_nm}'"))?;
    }

    // Chown the bind mounts sandbox needs to write to. `/nix/var/nix` is
    // *not* in this list anymore: the container runs Nix in multi-user mode
    // and the nix-daemon (started by the entrypoint script) owns all store
    // mutations on behalf of the sandbox user — chowning the daemon's state
    // dir would break it.
    //
    // /workspace and /home/sandbox/.claude are bind mounts. Apple's virtiofs
    // usually ignores chown on bind mounts (host owner shows through), so
    // these are best-effort — claude reads via 0644/0755 perms, and writes
    // go to the host as the host user.
    config
        .container
        .exec(
            &container_nm,
            "chown -R 1000:1000 /workspace /home/sandbox/.claude 2>/dev/null || true",
        )
        .with_context(|| format!("failed to chown sandbox dirs in '{container_nm}'"))?;

    // 6. Register branch in DB
    config
        .store
        .add_branch(project, branch, &sanitized)
        .with_context(|| format!("failed to register branch '{branch}' in store"))?;

    let proj = project.to_string();
    let br = branch.to_string();
    guard.push(move || {
        eprintln!("[rollback] note: branch registration for '{proj}/{br}' may need manual cleanup");
    });

    // All steps succeeded — commit the guard
    guard.commit();

    println!(
        "branch '{branch}' ready: container '{container_nm}' is running"
    );
    println!("run 'nixsand project attach {project} {branch}' to attach");

    Ok(())
}

// ---------------------------------------------------------------------------
// project attach
// ---------------------------------------------------------------------------

pub fn run_attach(config: &Config, project: &str, branch: &str) -> Result<()> {
    // Validate project + branch exist in the registry
    if !config.store.project_exists(project)? {
        bail!(
            "project '{project}' not found; run 'nixsand project add <git-url>' first"
        );
    }
    let sanitized = config
        .store
        .lookup_branch(project, branch)?
        .ok_or_else(|| {
            anyhow!(
                "branch '{branch}' not found for project '{project}'; run 'nixsand project branch {project} {branch}' first"
            )
        })?;

    let container_nm = container_name(project, &sanitized);
    let session = zmx_session_name(project, &sanitized);
    let worktree_in_container = format!("/workspace/worktrees/{branch}");

    // Start container if stopped
    if !config.container.container_running(&container_nm)? {
        eprintln!("[attach] container '{container_nm}' is stopped, starting...");
        config
            .container
            .start_container(&container_nm)
            .with_context(|| format!("failed to start container '{container_nm}'"))?;
    }

    // Build the exec command.
    //
    // Run as the `sandbox` user (uid 1000) — claude refuses
    // `--dangerously-skip-permissions` under uid 0. The container's entrypoint
    // has already chowned the bind mounts so sandbox can write to them.
    // claude lives in the system nix profile, which is on the default PATH.
    let exec_cmd = format!(
        "cd {worktree_in_container} && nix develop -c claude --dangerously-skip-permissions"
    );
    // Apple's `container exec --user <name>` hangs silently when given a
    // username string; only numeric UIDs work. The `sandbox` user is uid 1000
    // (see the base Dockerfile in src/image.rs).
    let tmux_cmd = format!(
        "container exec -it --user 1000 -e HOME=/home/sandbox {container_nm} sh -lc '{exec_cmd}'"
    );

    // Create tmux session if it doesn't exist; otherwise reattach
    if config.zmx.session_exists(&session)? {
        eprintln!("[attach] reattaching to existing session '{session}'");
        config
            .zmx
            .attach_session(&session)
            .with_context(|| format!("failed to attach to tmux session '{session}'"))?;
    } else {
        eprintln!("[attach] creating new tmux session '{session}'");
        config
            .zmx
            .new_session(&session, &tmux_cmd)
            .with_context(|| format!("failed to create tmux session '{session}'"))?;
        config
            .zmx
            .attach_session(&session)
            .with_context(|| format!("failed to attach to tmux session '{session}'"))?;
    }

    Ok(())
}
