use anyhow::{bail, Context, Result};

use crate::backend::BranchStatus;
use crate::cli::TaskStatus;
use crate::config::Config;
use crate::names::yeschef_session;
use crate::store::TicketRow;

// ---------------------------------------------------------------------------
// cleanup
// ---------------------------------------------------------------------------

/// Reap stale tickets. A ticket is reaped only when BOTH gates agree:
///
/// 1. **Git-status gate** — the branch is safe to reap: it is merged into the
///    project's main line, or its upstream was deleted on the remote. Branches
///    with unmerged work are always kept.
/// 2. **Task-status gate** — the cook is no longer working the ticket, i.e. it
///    self-reported `DONE`. An active ticket (`NEW`/`IN_PROGRESS`/`BLOCKED`, or
///    any unrecognized status) is kept even if its branch classifies as
///    merged/gone.
///
/// The second gate exists because the git status alone misfires on live work: a
/// freshly-spawned branch has no commits yet, so its tip still equals
/// `origin/main` and it classifies `Merged` — which would delete an
/// in-progress ticket out from under a running cook. Gating on the
/// self-reported status too means "if the cook says it's still working, we
/// don't reap it." When in doubt we keep: it is far better to leave a stale
/// ticket around than to delete active work.
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
            let git_status = config
                .git
                .branch_status(&bare_dir, &ticket.branch, &main_ref)
                .with_context(|| format!("failed to classify branch '{name}/{}'", ticket.branch))?;

            // Both gates must agree before we reap (see the module doc): the
            // branch must be safe to reap AND the cook must no longer be
            // active. Otherwise keep — and say exactly why.
            if !should_reap(git_status, &ticket.status) {
                let reason = keep_reason(git_status, &ticket.status);
                println!("  keep   {name}/{} — {reason}", ticket.branch);
                kept += 1;
                continue;
            }

            let reason = reap_reason(git_status, &ticket.status);
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

/// Whether a ticket's self-reported status means the cook still considers the
/// work live. An active ticket is never reaped, even if its branch classifies
/// as `Merged`/`Gone`.
///
/// Active (keep): `NEW` (just spawned — its tip may still equal `origin/main`,
/// the exact case that misfires as `Merged`), `IN_PROGRESS`, and `BLOCKED`
/// (stuck awaiting a decision, but still live work). Only `DONE` — the cook's
/// explicit "work finished, PR open" — is inactive and eligible for reaping.
/// Any *unrecognized* status is treated as active too: cleanup's failure mode
/// must be to keep, never to delete work we don't understand.
///
/// Window liveness (running/dead/gone) is intentionally NOT consulted. A gone
/// window only means the agent process exited — which happens both on a crash
/// mid-task (possibly with unpushed work) and on a clean finish — so it is not
/// a reliable "safe to delete" signal. The self-reported status is the cook's
/// explicit claim about the work, so we honor that instead: an active ticket
/// is kept even if its window is gone (re-spawn resumes it — the worktree and
/// status survive), and a `DONE` ticket is reaped whether its window is alive
/// or gone.
fn status_is_active(task_status: &str) -> bool {
    task_status != TaskStatus::Done.as_str()
}

/// The reap decision for one ticket: reap only when the branch is safe to reap
/// (`Merged`/`Gone`) AND the cook is no longer active. Both gates are
/// necessary; neither alone is sufficient.
fn should_reap(git_status: BranchStatus, task_status: &str) -> bool {
    match git_status {
        BranchStatus::Unmerged => false,
        BranchStatus::Merged | BranchStatus::Gone => !status_is_active(task_status),
    }
}

/// Human-readable label for how a branch relates to the main line.
fn git_status_label(git_status: BranchStatus) -> &'static str {
    match git_status {
        BranchStatus::Merged => "merged",
        BranchStatus::Gone => "gone from remote",
        BranchStatus::Unmerged => "unmerged work",
    }
}

/// Why a kept ticket was kept, for the dry-run / apply report.
fn keep_reason(git_status: BranchStatus, task_status: &str) -> String {
    match git_status {
        BranchStatus::Unmerged => git_status_label(git_status).to_string(),
        // Branch is reapable, so the cook's active status is what saved it.
        BranchStatus::Merged | BranchStatus::Gone => format!(
            "{}, but status {task_status} (active — not reaping)",
            git_status_label(git_status)
        ),
    }
}

/// Why a reaped ticket was reaped, for the dry-run / apply report.
fn reap_reason(git_status: BranchStatus, task_status: &str) -> String {
    format!("{} and status {task_status}", git_status_label(git_status))
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
    /// Fresh tickets take the `NEW` status default.
    fn register(h: &Harness, branch: &str) {
        let sanitized = crate::names::sanitize_branch(branch);
        let window = crate::names::window_name("proj", &sanitized);
        h.config
            .store
            .register_ticket("proj", branch, &sanitized, &window, "claude")
            .unwrap();
    }

    /// Register a ticket and set its self-reported task status.
    fn register_with_status(h: &Harness, branch: &str, status: &str) {
        register(h, branch);
        h.config
            .store
            .set_ticket_status("proj", branch, status)
            .unwrap();
    }

    #[test]
    fn reaps_merged_branch_and_deregisters() {
        let git = MockGitBackend::new().with_branch_status("done", BranchStatus::Merged);
        let h = harness(git);
        // Merged AND DONE — both gates agree, so it's reaped.
        register_with_status(&h, "done", "DONE");

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
        // Gone AND DONE — both gates agree, so it's reaped.
        register_with_status(&h, "landed", "DONE");

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
        // A genuine reap candidate (Merged + DONE) that the dry run must spare.
        register_with_status(&h, "done", "DONE");

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
        // The two reapable branches have finished cooks (DONE); wip-one is
        // unmerged and kept regardless of status.
        register_with_status(&h, "merged-one", "DONE");
        register_with_status(&h, "gone-one", "DONE");
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
        register_with_status(&h, "done", "DONE");

        run_cleanup(&h.config, None, true).unwrap();

        // Both projects were refreshed (fetch_prune per project).
        let git = h.git.recorded_calls();
        let fetches = git.iter().filter(|c| c.starts_with("fetch_prune:")).count();
        assert_eq!(fetches, 2, "git calls: {git:?}");
    }

    /// Assert that `branch` survived cleanup untouched: still registered, and no
    /// teardown (window kill / worktree removal) ran for it.
    fn assert_kept(h: &Harness, branch: &str) {
        assert!(
            h.config
                .store
                .lookup_ticket("proj", branch)
                .unwrap()
                .is_some(),
            "ticket '{branch}' must be kept in the registry"
        );
        let git = h.git.recorded_calls();
        assert!(
            !git.iter().any(|c| c.starts_with("remove_worktree:")),
            "active/unmerged work must not be removed; git calls: {git:?}"
        );
        let zmx = h.zmx.recorded_calls();
        assert!(
            !zmx.iter().any(|c| c.starts_with("kill_window:")),
            "active/unmerged work must not be killed; zmx calls: {zmx:?}"
        );
    }

    #[test]
    fn keeps_freshly_spawned_new_ticket_even_when_merged() {
        // The core bug: a just-spawned branch has no commits, so its tip equals
        // origin/main and it classifies `Merged`. Its status is the `NEW`
        // default. It must NOT be reaped out from under the running cook.
        let git = MockGitBackend::new().with_branch_status("fresh", BranchStatus::Merged);
        let h = harness(git);
        register(&h, "fresh"); // status defaults to NEW

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert_kept(&h, "fresh");
    }

    #[test]
    fn keeps_in_progress_ticket_even_when_merged() {
        let git = MockGitBackend::new().with_branch_status("wip", BranchStatus::Merged);
        let h = harness(git);
        register_with_status(&h, "wip", "IN_PROGRESS");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert_kept(&h, "wip");
    }

    #[test]
    fn keeps_blocked_ticket_even_when_gone() {
        let git = MockGitBackend::new().with_branch_status("stuck", BranchStatus::Gone);
        let h = harness(git);
        register_with_status(&h, "stuck", "BLOCKED");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert_kept(&h, "stuck");
    }

    #[test]
    fn keeps_unrecognized_status_even_when_merged() {
        // Safe failure mode: a status we don't understand is treated as active.
        let git = MockGitBackend::new().with_branch_status("weird", BranchStatus::Merged);
        let h = harness(git);
        register_with_status(&h, "weird", "SOME_FUTURE_STATUS");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert_kept(&h, "weird");
    }

    #[test]
    fn reaps_done_ticket_when_gone() {
        let git = MockGitBackend::new().with_branch_status("shipped", BranchStatus::Gone);
        let h = harness(git);
        register_with_status(&h, "shipped", "DONE");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert!(h
            .config
            .store
            .lookup_ticket("proj", "shipped")
            .unwrap()
            .is_none());
    }

    #[test]
    fn active_status_survives_alongside_reaped_done_ticket() {
        // Two branches both classify `Merged`; only the DONE one is reaped, the
        // IN_PROGRESS one is kept — the two gates are evaluated per-ticket.
        let git = MockGitBackend::new()
            .with_branch_status("finished", BranchStatus::Merged)
            .with_branch_status("working", BranchStatus::Merged);
        let h = harness(git);
        register_with_status(&h, "finished", "DONE");
        register_with_status(&h, "working", "IN_PROGRESS");

        run_cleanup(&h.config, Some("proj"), true).unwrap();

        assert!(h
            .config
            .store
            .lookup_ticket("proj", "finished")
            .unwrap()
            .is_none());
        assert!(h
            .config
            .store
            .lookup_ticket("proj", "working")
            .unwrap()
            .is_some());
    }

    #[test]
    fn should_reap_gates_on_both_git_and_task_status() {
        use BranchStatus::{Gone, Merged, Unmerged};

        // Unmerged is never reaped, whatever the task status.
        for status in ["NEW", "IN_PROGRESS", "BLOCKED", "DONE", "???"] {
            assert!(!should_reap(Unmerged, status), "unmerged/{status}");
        }
        // Merged/Gone are reaped only when the task status is inactive (DONE).
        for git in [Merged, Gone] {
            assert!(should_reap(git, "DONE"), "{git:?}/DONE should reap");
            for active in ["NEW", "IN_PROGRESS", "BLOCKED", "???"] {
                assert!(!should_reap(git, active), "{git:?}/{active} should keep");
            }
        }
    }

    #[test]
    fn reasons_explain_the_decision() {
        // Kept-because-unmerged, kept-because-active, and reaped all read clearly.
        assert_eq!(keep_reason(BranchStatus::Unmerged, "NEW"), "unmerged work");
        assert_eq!(
            keep_reason(BranchStatus::Merged, "IN_PROGRESS"),
            "merged, but status IN_PROGRESS (active — not reaping)"
        );
        assert_eq!(
            reap_reason(BranchStatus::Gone, "DONE"),
            "gone from remote and status DONE"
        );
    }
}
