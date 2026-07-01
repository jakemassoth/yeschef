use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};

pub struct Store {
    conn: Connection,
}

/// A registered ticket: a worktree + its zmx session + the agent launched in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketRow {
    pub project: String,
    pub branch: String,
    pub sanitized: String,
    pub window: String,
    pub agent: String,
    /// Self-reported task status (`NEW`/`IN_PROGRESS`/`DONE`/`BLOCKED`),
    /// orthogonal to zmx window liveness.
    pub status: String,
}

impl Store {
    /// Open a store backed by a file on disk.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    #[allow(dead_code)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory database")?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        // Enable WAL mode for file-backed databases (no-op for :memory:)
        self.conn
            .execute_batch("PRAGMA journal_mode=WAL;")
            .context("failed to set WAL mode")?;

        // `branches` is the ticket registry: one row per worktree, recording the
        // window (its backing zmx session) and the agent command launched in it.
        self.conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS projects (
                    name TEXT PRIMARY KEY,
                    git_url TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS branches (
                    project TEXT NOT NULL REFERENCES projects(name),
                    branch TEXT NOT NULL,
                    sanitized TEXT NOT NULL,
                    window TEXT NOT NULL DEFAULT '',
                    agent TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'NEW',
                    PRIMARY KEY (project, branch)
                );
                ",
            )
            .context("failed to initialize schema")?;

        // Migrate older DBs (pre-orchestration schema) that lack window/agent.
        // `ALTER TABLE ADD COLUMN` errors with "duplicate column" if present —
        // tolerate that so init stays idempotent.
        for stmt in [
            "ALTER TABLE branches ADD COLUMN window TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE branches ADD COLUMN agent TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE branches ADD COLUMN status TEXT NOT NULL DEFAULT 'NEW'",
        ] {
            if let Err(e) = self.conn.execute(stmt, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column") {
                    return Err(e).context("failed to migrate branches table");
                }
            }
        }

        Ok(())
    }

    /// Add a project to the registry. Errors if the name already exists.
    pub fn add_project(&self, name: &str, git_url: &str) -> Result<()> {
        let rows = self
            .conn
            .execute(
                "INSERT INTO projects (name, git_url) VALUES (?1, ?2)",
                params![name, git_url],
            )
            .with_context(|| {
                format!("failed to add project '{name}': name may already be taken")
            })?;
        if rows == 0 {
            return Err(anyhow!("project '{name}' already exists"));
        }
        Ok(())
    }

    /// List all projects. Returns (name, `git_url`) pairs.
    pub fn list_projects(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, git_url FROM projects ORDER BY name")
            .context("failed to prepare list query")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .context("failed to query projects")?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.context("failed to read project row")?);
        }
        Ok(result)
    }

    /// Check if a project exists.
    pub fn project_exists(&self, name: &str) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .context("failed to check project existence")?;
        Ok(count > 0)
    }

    /// Remove a project from the registry.
    #[allow(dead_code)]
    pub fn remove_project(&self, name: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM branches WHERE project = ?1", params![name])
            .with_context(|| format!("failed to remove tickets for project '{name}'"))?;
        self.conn
            .execute("DELETE FROM projects WHERE name = ?1", params![name])
            .with_context(|| format!("failed to remove project '{name}'"))?;
        Ok(())
    }

    /// Register (or update) a ticket: a worktree + its zmx session + agent.
    pub fn register_ticket(
        &self,
        project: &str,
        branch: &str,
        sanitized: &str,
        window: &str,
        agent: &str,
    ) -> Result<()> {
        // Upsert rather than INSERT OR REPLACE: a re-spawn (the supported
        // "resume" path) must refresh window/agent/sanitized but LEAVE the
        // self-reported `status` intact. A brand-new ticket takes the 'NEW'
        // column default.
        self.conn
            .execute(
                "INSERT INTO branches (project, branch, sanitized, window, agent)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(project, branch) DO UPDATE SET
                     sanitized = excluded.sanitized,
                     window = excluded.window,
                     agent = excluded.agent",
                params![project, branch, sanitized, window, agent],
            )
            .with_context(|| format!("failed to register ticket '{project}/{branch}'"))?;
        Ok(())
    }

    /// Set a ticket's self-reported task status. Errors if the ticket doesn't
    /// exist (never silently creates a row).
    pub fn set_ticket_status(&self, project: &str, branch: &str, status: &str) -> Result<()> {
        let rows = self
            .conn
            .execute(
                "UPDATE branches SET status = ?3 WHERE project = ?1 AND branch = ?2",
                params![project, branch, status],
            )
            .with_context(|| format!("failed to set status for ticket '{project}/{branch}'"))?;
        if rows == 0 {
            return Err(anyhow!("no ticket for '{project}/{branch}'"));
        }
        Ok(())
    }

    /// Look up a single ticket by project + branch.
    pub fn lookup_ticket(&self, project: &str, branch: &str) -> Result<Option<TicketRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project, branch, sanitized, window, agent, status
                 FROM branches WHERE project = ?1 AND branch = ?2",
            )
            .context("failed to prepare ticket lookup")?;
        let mut rows = stmt
            .query_map(params![project, branch], row_to_ticket)
            .context("failed to query ticket")?;
        match rows.next() {
            Some(row) => Ok(Some(row.context("failed to read ticket row")?)),
            None => Ok(None),
        }
    }

    /// List all registered tickets, ordered by project then branch.
    pub fn list_tickets(&self) -> Result<Vec<TicketRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project, branch, sanitized, window, agent, status
                 FROM branches ORDER BY project, branch",
            )
            .context("failed to prepare ticket list")?;
        let rows = stmt
            .query_map([], row_to_ticket)
            .context("failed to query tickets")?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.context("failed to read ticket row")?);
        }
        Ok(result)
    }

    /// Remove a ticket registration.
    pub fn remove_ticket(&self, project: &str, branch: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM branches WHERE project = ?1 AND branch = ?2",
                params![project, branch],
            )
            .with_context(|| format!("failed to remove ticket '{project}/{branch}'"))?;
        Ok(())
    }
}

fn row_to_ticket(row: &rusqlite::Row) -> rusqlite::Result<TicketRow> {
    Ok(TicketRow {
        project: row.get(0)?,
        branch: row.get(1)?,
        sanitized: row.get(2)?,
        window: row.get(3)?,
        agent: row.get(4)?,
        status: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    #[test]
    fn add_and_list_projects() {
        let s = store();
        s.add_project("foo", "https://example.com/foo.git").unwrap();
        s.add_project("bar", "https://example.com/bar.git").unwrap();
        let projects = s.list_projects().unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].0, "bar");
        assert_eq!(projects[1].0, "foo");
    }

    #[test]
    fn duplicate_project_name_errors() {
        let s = store();
        s.add_project("foo", "https://example.com/foo.git").unwrap();
        let result = s.add_project("foo", "https://example.com/other.git");
        assert!(result.is_err(), "duplicate project should fail");
    }

    #[test]
    fn register_and_lookup_ticket() {
        let s = store();
        s.add_project("myproject", "https://example.com/foo.git")
            .unwrap();
        s.register_ticket(
            "myproject",
            "feature/foo",
            "feature-foo",
            "myproject-feature-foo",
            "claude",
        )
        .unwrap();
        let ticket = s
            .lookup_ticket("myproject", "feature/foo")
            .unwrap()
            .unwrap();
        assert_eq!(ticket.sanitized, "feature-foo");
        assert_eq!(ticket.window, "myproject-feature-foo");
        assert_eq!(ticket.agent, "claude");
        // A brand-new ticket starts at the 'NEW' default.
        assert_eq!(ticket.status, "NEW");
    }

    #[test]
    fn set_ticket_status_updates_status() {
        let s = store();
        s.add_project("p", "https://example.com/p.git").unwrap();
        s.register_ticket("p", "a", "a", "p-a", "claude").unwrap();
        s.set_ticket_status("p", "a", "BLOCKED").unwrap();
        let ticket = s.lookup_ticket("p", "a").unwrap().unwrap();
        assert_eq!(ticket.status, "BLOCKED");
    }

    #[test]
    fn set_ticket_status_errors_for_unknown_ticket() {
        let s = store();
        s.add_project("p", "https://example.com/p.git").unwrap();
        let err = s.set_ticket_status("p", "ghost", "DONE").unwrap_err();
        assert!(err.to_string().contains("no ticket"), "{err}");
        // Must not have silently created a row.
        assert!(s.lookup_ticket("p", "ghost").unwrap().is_none());
    }

    #[test]
    fn reregister_preserves_status() {
        let s = store();
        s.add_project("p", "https://example.com/p.git").unwrap();
        s.register_ticket("p", "a", "a", "p-a", "claude").unwrap();
        s.set_ticket_status("p", "a", "IN_PROGRESS").unwrap();

        // Re-spawn: window/agent change, but the reported status survives.
        s.register_ticket("p", "a", "a", "p-a", "codex").unwrap();
        let ticket = s.lookup_ticket("p", "a").unwrap().unwrap();
        assert_eq!(ticket.agent, "codex");
        assert_eq!(ticket.status, "IN_PROGRESS");
    }

    #[test]
    fn lookup_nonexistent_ticket_returns_none() {
        let s = store();
        s.add_project("myproject", "https://example.com/foo.git")
            .unwrap();
        assert!(s.lookup_ticket("myproject", "nope").unwrap().is_none());
    }

    #[test]
    fn list_and_remove_tickets() {
        let s = store();
        s.add_project("p", "https://example.com/p.git").unwrap();
        s.register_ticket("p", "a", "a", "p-a", "claude").unwrap();
        s.register_ticket("p", "b", "b", "p-b", "codex").unwrap();
        assert_eq!(s.list_tickets().unwrap().len(), 2);
        s.remove_ticket("p", "a").unwrap();
        let tickets = s.list_tickets().unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].branch, "b");
    }
}
