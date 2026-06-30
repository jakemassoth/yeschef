use anyhow::{bail, Context, Result};

use crate::backend::BranchStatus;
use crate::config::Config;
use crate::names::yeschef_session;
use crate::store::TicketRow;

// ---------------------------------------------------------------------------
// cleanup
// ---------------------------------------------------------------------------

/// Reap stale tickets: those whose branch is merged into the project's main
/// line, or whose upstream was deleted on the remote. Branches with unmerged
/// work are left untouched.
///
/// With no `project`, every registered project is cleaned. Defaults to a dry
/// run (report only); pass `yes = true` to actually reap.
pub fn run_cleanup(config: &Config, project: Option<&str>, yes: bool) -> Result<()> {
    let projects = resolve_projects(config, project)?;
    if projects.is_empty() {
        println!("no projects registered; run 'yeschef project add <git-url>' to add one");
        return Ok(());
    }

    let session = yeschef_session();
    let mut reaped = 0usize;
    let mut kept = 0usize;

    for name in &projects {
        let bare_dir = config.bare_repo_dir(name);

        // Refresh first so the merged / gone determination reflects the latest
        // remote state (deleted branches are pruned, merges are visible).
        // `ensure_tracking_refspec` repairs clones that lack a fetch refspec so
        // `origin/<branch>` resolves and pruned upstreams report `[gone]`.
        config
            .git
            .ensure_tracking_refspec(&bare_dir)
            .with_context(|| format!("failed to configure tracking refspec for '{name}'"))?;
        config
            .git
            .fetch_prune(&bare_dir)
            .with_context(|| format!("failed to refresh project '{name}' before cleanup"))?;

        let default = config
            .git
            .default_branch(&bare_dir)
            .with_context(|| format!("failed to determine default branch for '{name}'"))?;
        let main_ref = format!("origin/{default}");

        let tickets: Vec<TicketRow> = config
            .store
            .list_tickets()?
            .into_iter()
            .filter(|t| t.project == *name)
            .collect();
        if tickets.is_empty() {
            continue;
        }

        println!("cleaning '{name}' (main line: {main_ref})...");
        for ticket in tickets {
            let status = config
                .git
                .branch_status(&bare_dir, &ticket.branch, &main_ref)
                .with_context(|| format!("failed to classify branch '{name}/{}'", ticket.branch))?;

            let reason = match status {
                BranchStatus::Merged => "merged",
                BranchStatus::Gone => "gone from remote",
                BranchStatus::Unmerged => {
                    println!("  keep   {name}/{} — unmerged work", ticket.branch);
                    kept += 1;
                    continue;
                }
            };

            if yes {
                reap_ticket(config, session, name, &ticket)?;
                println!("  reaped {name}/{} — {reason}", ticket.branch);
            } else {
                println!("  would reap {name}/{} — {reason}", ticket.branch);
            }
            reaped += 1;
        }
    }

    if yes {
        println!("done: {reaped} reaped, {kept} kept");
    } else {
        println!("dry run: {reaped} would be reaped, {kept} kept (re-run with --yes to apply)");
    }
    Ok(())
}

/// Resolve the set of projects to clean: a single named project (erroring if
/// unknown) or every registered project.
fn resolve_projects(config: &Config, project: Option<&str>) -> Result<Vec<String>> {
    match project {
        Some(name) => {
            if !config.store.project_exists(name)? {
                bail!("project '{name}' not found; run 'yeschef project add <git-url>' first");
            }
            Ok(vec![name.to_string()])
        }
        None => Ok(config
            .store
            .list_projects()?
            .into_iter()
            .map(|(name, _)| name)
            .collect()),
    }
}

/// Tear a ticket down: kill its zmx session, remove its worktree (pruning
/// stale metadata), delete the local branch, and deregister it. Each step is
/// idempotent, so a ticket whose session or worktree is already gone reaps
/// cleanly rather than erroring out.
fn reap_ticket(config: &Config, session: &str, project: &str, ticket: &TicketRow) -> Result<()> {
    config
        .zmx
        .kill_window(session, &ticket.window)
        .with_context(|| format!("failed to kill window for '{project}/{}'", ticket.branch))?;

    let bare_dir = config.bare_repo_dir(project);
    let worktree_path = config.worktree_dir(project, &ticket.branch);
    config
        .git
        .remove_worktree(&bare_dir, &worktree_path)
        .with_context(|| {
            format!(
                "failed to remove worktree for '{project}/{}'",
                ticket.branch
            )
        })?;
    config
        .git
        .delete_branch(&bare_dir, &ticket.branch)
        .with_context(|| format!("failed to delete branch for '{project}/{}'", ticket.branch))?;

    config
        .store
        .remove_ticket(project, &ticket.branch)
        .with_context(|| format!("failed to deregister ticket '{project}/{}'", ticket.branch))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::{MockGitBackend, MockZmxBackend};
    use crate::store::Store;
    use tempfile::TempDir;

    /// A Config backed by mocks + an in-memory store, with one project already
    /// registered. The mock handles share state with the copies inside
    /// `config` (they're `Arc`-backed), so calls are observable through them.
    /// Keep `_tmp` alive for the duration of the test.
    struct Harness {
        config: Config,
        zmx: MockZmxBackend,
        git: MockGitBackend,
        _tmp: TempDir,
    }

    fn harness(git: MockGitBackend) -> Harness {
        let tmp = TempDir::new().unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .add_project("proj", "https://example.com/proj.git")
            .unwrap();
        let zmx = MockZmxBackend::new();
        let config = Config {
            home: tmp.path().to_path_buf(),
            store,
            git: Box::new(git.clone()),
            zmx: Box::new(zmx.clone()),
        };
        Harness {
            config,
            zmx,
            git,
            _tmp: tmp,
        }
    }

    /// Register a ticket on `proj` in the store the same way `spawn` would.
    fn register(h: &Harness, branch: &str) {
        let sanitized = crate::names::sanitize_branch(branch);
        let window = crate::names::window_name("proj", &sanitized);
        h.config
            .store
            .register_ticket("proj", branch, &sanitized, &window, "claude")
            .unwrap();
    }

    #[test]
    fn reaps_merged_branch_and_deregisters() {
        let git = MockGitBackend::new().with_branch_status("done", BranchStatus::Merged);
        let h = harness(git);
        register(&h, "done");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        // Ticket is gone from the registry.
        assert!(h
            .config
            .store
            .lookup_ticket("proj", "done")
            .unwrap()
            .is_none());

        // The full teardown path ran: kill window, remove worktree, delete branch.
        let zmx = h.zmx.recorded_calls();
        assert!(
            zmx.contains(&"kill_window:yeschef:proj-done".to_string()),
            "zmx calls: {zmx:?}"
        );
        let git = h.git.recorded_calls();
        assert!(
            git.iter().any(|c| c.starts_with("remove_worktree:")),
            "git calls: {git:?}"
        );
        assert!(
            git.iter()
                .any(|c| c.starts_with("delete_branch:") && c.ends_with(":done")),
            "git calls: {git:?}"
        );
    }

    #[test]
    fn reaps_gone_branch() {
        let git = MockGitBackend::new().with_branch_status("landed", BranchStatus::Gone);
        let h = harness(git);
        register(&h, "landed");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert!(h
            .config
            .store
            .lookup_ticket("proj", "landed")
            .unwrap()
            .is_none());
    }

    #[test]
    fn keeps_unmerged_branch() {
        // No status seeded → classified Unmerged (the safe default).
        let h = harness(MockGitBackend::new());
        register(&h, "wip");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        // Ticket survives, and no teardown ran for it.
        assert!(h
            .config
            .store
            .lookup_ticket("proj", "wip")
            .unwrap()
            .is_some());
        let git = h.git.recorded_calls();
        assert!(
            !git.iter().any(|c| c.starts_with("remove_worktree:")),
            "unmerged work must not be removed; git calls: {git:?}"
        );
        let zmx = h.zmx.recorded_calls();
        assert!(
            !zmx.iter().any(|c| c.starts_with("kill_window:")),
            "unmerged work must not be killed; zmx calls: {zmx:?}"
        );
    }

    #[test]
    fn dry_run_removes_nothing() {
        let git = MockGitBackend::new().with_branch_status("done", BranchStatus::Merged);
        let h = harness(git);
        register(&h, "done");

        run_cleanup(&h.config, Some("proj"), false).unwrap();

        // Default (no --yes) is a dry run: the ticket and its resources stay.
        assert!(h
            .config
            .store
            .lookup_ticket("proj", "done")
            .unwrap()
            .is_some());
        let git = h.git.recorded_calls();
        assert!(
            git.iter().any(|c| c.starts_with("branch_status:")),
            "dry run should still classify branches; git calls: {git:?}"
        );
        assert!(
            !git.iter().any(|c| c.starts_with("remove_worktree:")),
            "dry run must not remove worktrees; git calls: {git:?}"
        );
        assert!(!h
            .zmx
            .recorded_calls()
            .iter()
            .any(|c| c.starts_with("kill_window:")));
    }

    #[test]
    fn refreshes_before_classifying() {
        let git = MockGitBackend::new().with_branch_status("done", BranchStatus::Merged);
        let h = harness(git);
        register(&h, "done");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        let git = h.git.recorded_calls();
        let fetch_idx = git.iter().position(|c| c.starts_with("fetch_prune:"));
        let status_idx = git.iter().position(|c| c.starts_with("branch_status:"));
        assert!(
            fetch_idx.is_some(),
            "expected a fetch_prune; calls: {git:?}"
        );
        assert!(
            fetch_idx < status_idx,
            "fetch must precede classification; calls: {git:?}"
        );
    }

    #[test]
    fn classifies_against_origin_main() {
        let git = MockGitBackend::new().with_branch_status("done", BranchStatus::Merged);
        let h = harness(git);
        register(&h, "done");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        let git = h.git.recorded_calls();
        assert!(
            git.iter()
                .any(|c| c.starts_with("branch_status:") && c.ends_with(":origin/main")),
            "should classify against the remote main tip; calls: {git:?}"
        );
    }

    #[test]
    fn mixed_tickets_reap_only_the_safe_ones() {
        let git = MockGitBackend::new()
            .with_branch_status("merged-one", BranchStatus::Merged)
            .with_branch_status("gone-one", BranchStatus::Gone)
            .with_branch_status("wip-one", BranchStatus::Unmerged);
        let h = harness(git);
        register(&h, "merged-one");
        register(&h, "gone-one");
        register(&h, "wip-one");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert!(h
            .config
            .store
            .lookup_ticket("proj", "merged-one")
            .unwrap()
            .is_none());
        assert!(h
            .config
            .store
            .lookup_ticket("proj", "gone-one")
            .unwrap()
            .is_none());
        assert!(h
            .config
            .store
            .lookup_ticket("proj", "wip-one")
            .unwrap()
            .is_some());
    }

    #[test]
    fn unknown_project_errors() {
        let h = harness(MockGitBackend::new());
        let err = run_cleanup(&h.config, Some("nope"), true).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn all_projects_are_cleaned_when_none_named() {
        let git = MockGitBackend::new().with_branch_status("done", BranchStatus::Merged);
        let h = harness(git);
        h.config
            .store
            .add_project("other", "https://example.com/other.git")
            .unwrap();
        register(&h, "done");

        run_cleanup(&h.config, None, true).unwrap();

        // Both projects were refreshed (fetch_prune per project).
        let git = h.git.recorded_calls();
        let fetches = git.iter().filter(|c| c.starts_with("fetch_prune:")).count();
        assert_eq!(fetches, 2, "git calls: {git:?}");
    }
}
