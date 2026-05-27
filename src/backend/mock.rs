#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};

use super::{ContainerBackend, GitBackend, Mount, ZmxBackend};

// ---------------------------------------------------------------------------
// Recording mock for ContainerBackend
// ---------------------------------------------------------------------------

/// A recording mock for `ContainerBackend`.
/// Stores calls as strings (e.g. "build_image:nixsand-base") and returns
/// pre-configured results.
#[derive(Clone, Default)]
pub struct MockContainerBackend {
    pub calls: Arc<Mutex<Vec<String>>>,
    /// images that "exist" — checked by `image_exists`
    pub existing_images: Arc<Mutex<Vec<String>>>,
    /// containers that "exist"
    pub existing_containers: Arc<Mutex<Vec<String>>>,
    /// containers that are "running"
    pub running_containers: Arc<Mutex<Vec<String>>>,
    /// If set, `build_image` will return this error for the given tag
    pub build_errors: Arc<Mutex<HashMap<String, String>>>,
}

impl MockContainerBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_existing_images(images: &[&str]) -> Self {
        let m = Self::new();
        let mut lock = m.existing_images.lock().unwrap();
        for img in images {
            lock.push(img.to_string());
        }
        drop(lock);
        m
    }

    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl ContainerBackend for MockContainerBackend {
    fn image_exists(&self, tag: &str) -> Result<bool> {
        self.record(format!("image_exists:{tag}"));
        let images = self.existing_images.lock().unwrap();
        Ok(images.contains(&tag.to_string()))
    }

    fn build_image(&self, tag: &str, _context_dir: &Path) -> Result<()> {
        self.record(format!("build_image:{tag}"));
        let errors = self.build_errors.lock().unwrap();
        if let Some(err) = errors.get(tag) {
            bail!("{err}");
        }
        // Add to existing images on success
        drop(errors);
        self.existing_images
            .lock()
            .unwrap()
            .push(tag.to_string());
        Ok(())
    }

    fn container_exists(&self, name: &str) -> Result<bool> {
        self.record(format!("container_exists:{name}"));
        let containers = self.existing_containers.lock().unwrap();
        Ok(containers.contains(&name.to_string()))
    }

    fn container_running(&self, name: &str) -> Result<bool> {
        self.record(format!("container_running:{name}"));
        let running = self.running_containers.lock().unwrap();
        Ok(running.contains(&name.to_string()))
    }

    fn create_container(
        &self,
        name: &str,
        image: &str,
        _mounts: &[Mount],
        _entrypoint: &[&str],
    ) -> Result<()> {
        self.record(format!("create_container:{name}:{image}"));
        self.existing_containers
            .lock()
            .unwrap()
            .push(name.to_string());
        Ok(())
    }

    fn start_container(&self, name: &str) -> Result<()> {
        self.record(format!("start_container:{name}"));
        self.running_containers
            .lock()
            .unwrap()
            .push(name.to_string());
        Ok(())
    }

    fn remove_container(&self, name: &str) -> Result<()> {
        self.record(format!("remove_container:{name}"));
        self.existing_containers
            .lock()
            .unwrap()
            .retain(|c| c != name);
        self.running_containers
            .lock()
            .unwrap()
            .retain(|c| c != name);
        Ok(())
    }

    fn exec_interactive(&self, name: &str, command: &str) -> Result<()> {
        self.record(format!("exec_interactive:{name}:{command}"));
        Ok(())
    }

    fn exec(&self, name: &str, command: &str) -> Result<()> {
        self.record(format!("exec:{name}:{command}"));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Recording mock for GitBackend
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct MockGitBackend {
    pub calls: Arc<Mutex<Vec<String>>>,
    /// Content to return for `read_file` requests, keyed by "`repo_path:file_path`"
    pub file_contents: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub default_branch_response: Arc<Mutex<String>>,
}

impl MockGitBackend {
    pub fn new() -> Self {
        let m = Self::default();
        *m.default_branch_response.lock().unwrap() = "main".to_string();
        m
    }

    pub fn with_file(self, file_path: &str, content: &[u8]) -> Self {
        self.file_contents
            .lock()
            .unwrap()
            .insert(file_path.to_string(), content.to_vec());
        self
    }

    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl GitBackend for MockGitBackend {
    fn clone_bare(&self, url: &str, dest: &Path) -> Result<()> {
        self.record(format!("clone_bare:{}:{}", url, dest.display()));
        Ok(())
    }

    fn set_config(&self, repo: &Path, key: &str, value: &str) -> Result<()> {
        self.record(format!("set_config:{}:{}:{}", repo.display(), key, value));
        Ok(())
    }

    fn unset_config(&self, repo: &Path, key: &str) -> Result<()> {
        self.record(format!("unset_config:{}:{}", repo.display(), key));
        Ok(())
    }

    fn add_worktree(
        &self,
        bare_repo: &Path,
        worktree_path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<()> {
        self.record(format!(
            "add_worktree:{}:{}:{}:{}",
            bare_repo.display(),
            worktree_path.display(),
            branch,
            base
        ));
        Ok(())
    }

    fn default_branch(&self, bare_repo: &Path) -> Result<String> {
        self.record(format!("default_branch:{}", bare_repo.display()));
        Ok(self.default_branch_response.lock().unwrap().clone())
    }

    fn read_file(&self, _repo: &Path, path: &str) -> Result<Vec<u8>> {
        self.record(format!("read_file:{path}"));
        let contents = self.file_contents.lock().unwrap();
        Ok(contents.get(path).cloned().unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Recording mock for ZmxBackend
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct MockZmxBackend {
    pub calls: Arc<Mutex<Vec<String>>>,
    pub existing_sessions: Arc<Mutex<Vec<String>>>,
}

impl MockZmxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl ZmxBackend for MockZmxBackend {
    fn session_exists(&self, session: &str) -> Result<bool> {
        self.record(format!("session_exists:{session}"));
        let sessions = self.existing_sessions.lock().unwrap();
        Ok(sessions.contains(&session.to_string()))
    }

    fn new_session(&self, session: &str, command: &str) -> Result<()> {
        self.record(format!("new_session:{session}:{command}"));
        self.existing_sessions
            .lock()
            .unwrap()
            .push(session.to_string());
        Ok(())
    }

    fn attach_session(&self, session: &str) -> Result<()> {
        self.record(format!("attach_session:{session}"));
        Ok(())
    }
}
