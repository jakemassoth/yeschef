use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};

pub struct Store {
    conn: Connection,
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
        let conn = Connection::open_in_memory()
            .context("failed to open in-memory database")?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        // Enable WAL mode for file-backed databases (no-op for :memory:)
        self.conn
            .execute_batch("PRAGMA journal_mode=WAL;")
            .context("failed to set WAL mode")?;

        self.conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS projects (
                    name TEXT PRIMARY KEY,
                    git_url TEXT NOT NULL,
                    flake_lock_hash TEXT
                );
                CREATE TABLE IF NOT EXISTS branches (
                    project TEXT NOT NULL REFERENCES projects(name),
                    branch TEXT NOT NULL,
                    sanitized TEXT NOT NULL,
                    PRIMARY KEY (project, branch)
                );
                ",
            )
            .context("failed to initialize schema")?;

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

    /// Add a branch registration.
    pub fn add_branch(&self, project: &str, branch: &str, sanitized: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO branches (project, branch, sanitized) VALUES (?1, ?2, ?3)",
                params![project, branch, sanitized],
            )
            .with_context(|| format!("failed to add branch '{branch}' for project '{project}'"))?;
        Ok(())
    }

    /// Look up a branch registration. Returns sanitized branch name if found.
    pub fn lookup_branch(&self, project: &str, branch: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT sanitized FROM branches WHERE project = ?1 AND branch = ?2")
            .context("failed to prepare branch lookup")?;
        let mut rows = stmt
            .query_map(params![project, branch], |row| row.get(0))
            .context("failed to query branch")?;
        match rows.next() {
            Some(row) => Ok(Some(row.context("failed to read branch row")?)),
            None => Ok(None),
        }
    }

    /// Remove a branch registration.
    #[allow(dead_code)]
    pub fn remove_branch(&self, project: &str, branch: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM branches WHERE project = ?1 AND branch = ?2",
                params![project, branch],
            )
            .with_context(|| {
                format!("failed to remove branch '{branch}' for project '{project}'")
            })?;
        Ok(())
    }

    /// Record the flake.lock hash for a project.
    pub fn set_flake_lock_hash(&self, project: &str, hash: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE projects SET flake_lock_hash = ?1 WHERE name = ?2",
                params![hash, project],
            )
            .with_context(|| format!("failed to update flake.lock hash for project '{project}'"))?;
        Ok(())
    }

    /// Get the recorded flake.lock hash for a project.
    pub fn get_flake_lock_hash(&self, project: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT flake_lock_hash FROM projects WHERE name = ?1")
            .context("failed to prepare flake.lock hash query")?;
        let mut rows = stmt
            .query_map(params![project], |row| row.get(0))
            .context("failed to query flake.lock hash")?;
        match rows.next() {
            Some(row) => Ok(row.context("failed to read flake.lock hash row")?),
            None => Ok(None),
        }
    }
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
    fn add_and_lookup_branch() {
        let s = store();
        s.add_project("myproject", "https://example.com/foo.git")
            .unwrap();
        s.add_branch("myproject", "feature/foo", "feature-foo").unwrap();
        let sanitized = s.lookup_branch("myproject", "feature/foo").unwrap();
        assert_eq!(sanitized, Some("feature-foo".to_string()));
    }

    #[test]
    fn lookup_nonexistent_branch_returns_none() {
        let s = store();
        s.add_project("myproject", "https://example.com/foo.git")
            .unwrap();
        let result = s.lookup_branch("myproject", "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn flake_lock_hash_roundtrip() {
        let s = store();
        s.add_project("myproject", "https://example.com/foo.git")
            .unwrap();
        // Initially None
        assert_eq!(s.get_flake_lock_hash("myproject").unwrap(), None);
        // Set and retrieve
        s.set_flake_lock_hash("myproject", "abc123").unwrap();
        assert_eq!(
            s.get_flake_lock_hash("myproject").unwrap(),
            Some("abc123".to_string())
        );
        // Update
        s.set_flake_lock_hash("myproject", "def456").unwrap();
        assert_eq!(
            s.get_flake_lock_hash("myproject").unwrap(),
            Some("def456".to_string())
        );
    }
}
