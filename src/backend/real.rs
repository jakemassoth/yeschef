use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::{ContainerBackend, GitBackend, Mount, ZmxBackend};

// ---------------------------------------------------------------------------
// ContainerBackend — wraps Apple's `container` CLI
// ---------------------------------------------------------------------------

pub struct RealContainerBackend;

impl ContainerBackend for RealContainerBackend {
    fn image_exists(&self, tag: &str) -> Result<bool> {
        let output = Command::new("container")
            .args(["image", "inspect", tag])
            .output()
            .context("failed to run 'container image inspect'")?;
        Ok(output.status.success())
    }

    fn build_image(&self, tag: &str, context_dir: &Path) -> Result<()> {
        let status = Command::new("container")
            .args(["build", "-t", tag])
            .arg(context_dir)
            .status()
            .context("failed to run 'container build'")?;
        if !status.success() {
            bail!("container build failed for image '{tag}'");
        }
        Ok(())
    }

    fn container_exists(&self, name: &str) -> Result<bool> {
        // Apple's `container inspect` returns [] with exit 0 for missing containers;
        // use `container list --all` instead.
        let output = Command::new("container")
            .args(["list", "--all"])
            .output()
            .context("failed to run 'container list --all'")?;
        if !output.status.success() {
            return Ok(false);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains(name))
    }

    fn container_running(&self, name: &str) -> Result<bool> {
        // `container list` (no --all) shows only running containers.
        let output = Command::new("container")
            .args(["list"])
            .output()
            .context("failed to run 'container list'")?;
        if !output.status.success() {
            return Ok(false);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains(name))
    }

    fn create_container(
        &self,
        name: &str,
        image: &str,
        mounts: &[Mount],
        entrypoint: &[&str],
    ) -> Result<()> {
        let mut cmd = Command::new("container");
        cmd.args(["create", "--name", name]);
        for mount in mounts {
            cmd.arg("-v");
            cmd.arg(format!("{}:{}", mount.host_path, mount.container_path));
        }
        for (i, part) in entrypoint.iter().enumerate() {
            if i == 0 {
                cmd.arg("--entrypoint").arg(part);
            }
        }
        cmd.arg(image);
        // Pass remaining entrypoint args after the image
        if entrypoint.len() > 1 {
            cmd.args(&entrypoint[1..]);
        }

        let status = cmd
            .status()
            .context("failed to run 'container create'")?;
        if !status.success() {
            bail!("container create failed for '{name}'");
        }
        Ok(())
    }

    fn start_container(&self, name: &str) -> Result<()> {
        let output = Command::new("container")
            .args(["start", name])
            .output()
            .context("failed to run 'container start'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("container start failed for '{}': {}", name, stderr.trim());
        }
        Ok(())
    }

    fn remove_container(&self, name: &str) -> Result<()> {
        let status = Command::new("container")
            .args(["rm", "-f", name])
            .status()
            .context("failed to run 'container rm'")?;
        if !status.success() {
            bail!("container rm failed for '{name}'");
        }
        Ok(())
    }

    fn exec_interactive(&self, name: &str, command: &str) -> Result<()> {
        let status = Command::new("container")
            .args(["exec", "-it", name, "sh", "-lc", command])
            .status()
            .context("failed to run 'container exec'")?;
        if !status.success() {
            bail!("container exec failed for '{name}'");
        }
        Ok(())
    }

    fn exec(&self, name: &str, command: &str) -> Result<()> {
        let output = Command::new("container")
            .args(["exec", name, "sh", "-c", command])
            .output()
            .context("failed to run 'container exec'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("container exec failed for '{}': {}", name, stderr.trim());
        }
        Ok(())
    }
}

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

        let status = cmd
            .status()
            .context("failed to run 'git worktree add'")?;
        if !status.success() {
            bail!("git worktree add failed for branch '{branch}' from '{base}'");
        }
        Ok(())
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

    fn read_file(&self, repo: &Path, path: &str) -> Result<Vec<u8>> {
        let output = Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["show", &format!("HEAD:{path}")])
            .output()
            .context("failed to run 'git show'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git show HEAD:{} failed: {}", path, stderr.trim());
        }
        Ok(output.stdout)
    }
}

// ---------------------------------------------------------------------------
// ZmxBackend — wraps `tmux`
// ---------------------------------------------------------------------------

pub struct RealZmxBackend;

impl ZmxBackend for RealZmxBackend {
    fn session_exists(&self, session: &str) -> Result<bool> {
        let output = Command::new("tmux")
            .args(["has-session", "-t", session])
            .output()
            .context("failed to run 'tmux has-session'")?;
        Ok(output.status.success())
    }

    fn new_session(&self, session: &str, command: &str) -> Result<()> {
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", session, command])
            .status()
            .context("failed to run 'tmux new-session'")?;
        if !status.success() {
            bail!("tmux new-session failed for '{session}'");
        }
        Ok(())
    }

    fn attach_session(&self, session: &str) -> Result<()> {
        let status = Command::new("tmux")
            .args(["attach-session", "-t", session])
            .status()
            .context("failed to run 'tmux attach-session'")?;
        if !status.success() {
            bail!("tmux attach-session failed for '{session}'");
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
            "required dependency '{name}' not found in PATH; please install it before using nixsand"
        );
    }
    Ok(())
}
