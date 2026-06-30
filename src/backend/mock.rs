#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::{GitBackend, WindowInfo, ZmxBackend};

// ---------------------------------------------------------------------------
// Recording mock for GitBackend
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct MockGitBackend {
    pub calls: Arc<Mutex<Vec<String>>>,
    pub default_branch_response: Arc<Mutex<String>>,
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

    fn default_branch(&self, bare_repo: &Path) -> Result<String> {
        self.record(format!("default_branch:{}", bare_repo.display()));
        Ok(self.default_branch_response.lock().unwrap().clone())
    }

    fn fetch_prune(&self, bare_repo: &Path) -> Result<()> {
        self.record(format!("fetch_prune:{}", bare_repo.display()));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Recording mock for ZmxBackend
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct MockZmxBackend {
    pub calls: Arc<Mutex<Vec<String>>>,
    pub existing_sessions: Arc<Mutex<Vec<String>>>,
    /// In-memory windows per session, so orchestration logic can be tested
    /// without a real zmx. Keyed by session; value is the ordered window list.
    pub windows: Arc<Mutex<HashMap<String, Vec<WindowInfo>>>>,
    /// Canned pane content keyed by "`session:window`".
    pub pane_contents: Arc<Mutex<HashMap<String, String>>>,
}

impl MockZmxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn with_pane(self, session: &str, window: &str, content: &str) -> Self {
        self.pane_contents
            .lock()
            .unwrap()
            .insert(format!("{session}:{window}"), content.to_string());
        self
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl ZmxBackend for MockZmxBackend {
    fn session_exists(&self, session: &str) -> Result<bool> {
        self.record(format!("session_exists:{session}"));
        Ok(self
            .existing_sessions
            .lock()
            .unwrap()
            .contains(&session.to_string()))
    }

    fn ensure_session(&self, session: &str) -> Result<()> {
        self.record(format!("ensure_session:{session}"));
        let mut sessions = self.existing_sessions.lock().unwrap();
        if !sessions.contains(&session.to_string()) {
            sessions.push(session.to_string());
            self.windows
                .lock()
                .unwrap()
                .entry(session.to_string())
                .or_default();
        }
        Ok(())
    }

    fn new_window(&self, session: &str, window: &str, cwd: &Path, command: &str) -> Result<()> {
        self.record(format!(
            "new_window:{}:{}:{}:{}",
            session,
            window,
            cwd.display(),
            command
        ));
        self.windows
            .lock()
            .unwrap()
            .entry(session.to_string())
            .or_default()
            .push(WindowInfo {
                name: window.to_string(),
                active: true,
                dead: false,
            });
        Ok(())
    }

    fn window_exists(&self, session: &str, window: &str) -> Result<bool> {
        self.record(format!("window_exists:{session}:{window}"));
        Ok(self
            .windows
            .lock()
            .unwrap()
            .get(session)
            .is_some_and(|ws| ws.iter().any(|w| w.name == window)))
    }

    fn send_keys(&self, session: &str, window: &str, text: &str) -> Result<()> {
        self.record(format!("send_keys:{session}:{window}:{text}"));
        Ok(())
    }

    fn capture_pane(&self, session: &str, window: &str, lines: Option<usize>) -> Result<String> {
        self.record(format!(
            "capture_pane:{session}:{window}:{}",
            lines.map_or_else(|| "all".to_string(), |n| n.to_string())
        ));
        Ok(self
            .pane_contents
            .lock()
            .unwrap()
            .get(&format!("{session}:{window}"))
            .cloned()
            .unwrap_or_default())
    }

    fn list_windows(&self, session: &str) -> Result<Vec<WindowInfo>> {
        self.record(format!("list_windows:{session}"));
        Ok(self
            .windows
            .lock()
            .unwrap()
            .get(session)
            .cloned()
            .unwrap_or_default())
    }

    fn kill_window(&self, session: &str, window: &str) -> Result<()> {
        self.record(format!("kill_window:{session}:{window}"));
        if let Some(ws) = self.windows.lock().unwrap().get_mut(session) {
            ws.retain(|w| w.name != window);
        }
        Ok(())
    }

    fn attach(&self, session: &str, window: Option<&str>) -> Result<()> {
        self.record(format!("attach:{session}:{}", window.unwrap_or("-")));
        Ok(())
    }
}
