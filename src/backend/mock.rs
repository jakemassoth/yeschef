#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::{BranchStatus, GitBackend, HerdrBackend, Workspace, WorkspaceInfo};

// ---------------------------------------------------------------------------
// Recording mock for GitBackend
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct MockGitBackend {
    pub calls: Arc<Mutex<Vec<String>>>,
    pub default_branch_response: Arc<Mutex<String>>,
    /// Canned `branch_status` responses keyed by branch name. Branches with no
    /// entry classify as `Unmerged` (the safe default — keep).
    pub branch_statuses: Arc<Mutex<HashMap<String, BranchStatus>>>,
}

impl MockGitBackend {
    pub fn new() -> Self {
        let m = Self::default();
        *m.default_branch_response.lock().unwrap() = "main".to_string();
        m
    }

    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Pre-seed the classification a given branch reports from `branch_status`.
    pub fn with_branch_status(self, branch: &str, status: BranchStatus) -> Self {
        self.branch_statuses
            .lock()
            .unwrap()
            .insert(branch.to_string(), status);
        self
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

    fn remove_worktree(&self, bare_repo: &Path, worktree_path: &Path) -> Result<()> {
        self.record(format!(
            "remove_worktree:{}:{}",
            bare_repo.display(),
            worktree_path.display()
        ));
        Ok(())
    }

    fn delete_branch(&self, bare_repo: &Path, branch: &str) -> Result<()> {
        self.record(format!("delete_branch:{}:{}", bare_repo.display(), branch));
        Ok(())
    }

    fn default_branch(&self, bare_repo: &Path) -> Result<String> {
        self.record(format!("default_branch:{}", bare_repo.display()));
        Ok(self.default_branch_response.lock().unwrap().clone())
    }

    fn ensure_tracking_refspec(&self, bare_repo: &Path) -> Result<()> {
        self.record(format!("ensure_tracking_refspec:{}", bare_repo.display()));
        Ok(())
    }

    fn fetch_prune(&self, bare_repo: &Path) -> Result<()> {
        self.record(format!("fetch_prune:{}", bare_repo.display()));
        Ok(())
    }

    fn branch_status(
        &self,
        bare_repo: &Path,
        branch: &str,
        main_ref: &str,
    ) -> Result<BranchStatus> {
        self.record(format!(
            "branch_status:{}:{}:{}",
            bare_repo.display(),
            branch,
            main_ref
        ));
        Ok(self
            .branch_statuses
            .lock()
            .unwrap()
            .get(branch)
            .copied()
            .unwrap_or(BranchStatus::Unmerged))
    }
}

// ---------------------------------------------------------------------------
// Recording mock for HerdrBackend
// ---------------------------------------------------------------------------

/// One workspace in the mock's in-memory herdr model.
#[derive(Clone)]
struct MockWorkspace {
    workspace_id: String,
    label: String,
    pane_id: String,
    agent_status: String,
}

#[derive(Clone, Default)]
pub struct MockHerdrBackend {
    pub calls: Arc<Mutex<Vec<String>>>,
    /// Whether the brigade server is "running". `ensure_server` flips it on;
    /// `stop_server` flips it off — mirroring the real lifecycle.
    pub running: Arc<Mutex<bool>>,
    /// In-memory workspaces, so orchestration logic is testable without a real
    /// herdr server. Ordered by creation.
    workspaces: Arc<Mutex<Vec<MockWorkspace>>>,
    /// Canned pane content keyed by pane id (what `read_pane` returns).
    pub pane_contents: Arc<Mutex<HashMap<String, String>>>,
    /// Display-status metadata set per pane via `set_display_status`.
    pub display_statuses: Arc<Mutex<HashMap<String, String>>>,
    /// Monotonic counter minting workspace/pane ids (`w1`, `w1:p1`, …).
    next_id: Arc<Mutex<u32>>,
}

impl MockHerdrBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Pre-seed a pane's readable content by id. Ids are minted deterministically
    /// (`w1:p1` for the first workspace created, `w2:p1` for the second, …).
    pub fn with_pane(self, pane_id: &str, content: &str) -> Self {
        self.pane_contents
            .lock()
            .unwrap()
            .insert(pane_id.to_string(), content.to_string());
        self
    }

    /// The display status last set on a pane via `set_display_status`, if any.
    pub fn display_status(&self, pane_id: &str) -> Option<String> {
        self.display_statuses.lock().unwrap().get(pane_id).cloned()
    }

    /// The label of a workspace by id, if it exists.
    pub fn workspace_label(&self, workspace_id: &str) -> Option<String> {
        self.workspaces
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.workspace_id == workspace_id)
            .map(|w| w.label.clone())
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl HerdrBackend for MockHerdrBackend {
    fn ensure_server(&self) -> Result<()> {
        self.record("ensure_server".to_string());
        *self.running.lock().unwrap() = true;
        Ok(())
    }

    fn server_running(&self) -> Result<bool> {
        Ok(*self.running.lock().unwrap())
    }

    fn stop_server(&self) -> Result<()> {
        self.record("stop_server".to_string());
        *self.running.lock().unwrap() = false;
        Ok(())
    }

    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<Workspace> {
        self.record(format!("create_workspace:{label}:{}", cwd.display()));
        let mut n = self.next_id.lock().unwrap();
        *n += 1;
        let workspace_id = format!("w{n}");
        let pane_id = format!("{workspace_id}:p1");
        self.workspaces.lock().unwrap().push(MockWorkspace {
            workspace_id: workspace_id.clone(),
            label: label.to_string(),
            pane_id: pane_id.clone(),
            agent_status: "unknown".to_string(),
        });
        Ok(Workspace {
            workspace_id,
            pane_id,
        })
    }

    fn run_in_pane(&self, pane_id: &str, text: &str) -> Result<()> {
        self.record(format!("run_in_pane:{pane_id}:{text}"));
        Ok(())
    }

    fn read_pane(&self, pane_id: &str, lines: Option<usize>) -> Result<String> {
        self.record(format!(
            "read_pane:{pane_id}:{}",
            lines.map_or_else(|| "all".to_string(), |n| n.to_string())
        ));
        Ok(self
            .pane_contents
            .lock()
            .unwrap()
            .get(pane_id)
            .cloned()
            .unwrap_or_default())
    }

    fn set_display_status(&self, pane_id: &str, status: &str) -> Result<()> {
        self.record(format!("set_display_status:{pane_id}:{status}"));
        self.display_statuses
            .lock()
            .unwrap()
            .insert(pane_id.to_string(), status.to_string());
        Ok(())
    }

    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        self.record("list_workspaces".to_string());
        if !*self.running.lock().unwrap() {
            return Ok(Vec::new());
        }
        Ok(self
            .workspaces
            .lock()
            .unwrap()
            .iter()
            .map(|w| WorkspaceInfo {
                workspace_id: w.workspace_id.clone(),
                label: w.label.clone(),
                agent_status: w.agent_status.clone(),
            })
            .collect())
    }

    fn close_workspace(&self, workspace_id: &str) -> Result<()> {
        self.record(format!("close_workspace:{workspace_id}"));
        self.workspaces
            .lock()
            .unwrap()
            .retain(|w| w.workspace_id != workspace_id);
        Ok(())
    }

    fn attach(&self, workspace_id: Option<&str>) -> Result<()> {
        self.record(format!("attach:{}", workspace_id.unwrap_or("-")));
        Ok(())
    }
}
