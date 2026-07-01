use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

use crate::cli::TaskStatus;
use crate::config::Config;
use crate::guard::RollbackGuard;
use crate::names::{sanitize_branch, window_name, yeschef_session};
use crate::store::TicketRow;

/// Default number of pane lines `peek` returns.
const PEEK_LINES: usize = 40;

/// Wrap a string in single quotes for safe inclusion in a `sh -lc` command,
/// escaping any embedded single quotes.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Write the spawn prompt to a stable per-ticket file outside the worktree and
/// return its absolute path. Keyed by project/sanitized-branch so a re-spawn
/// overwrites rather than accumulating stale files.
fn write_prompt_file(
    config: &Config,
    project: &str,
    sanitized: &str,
    prompt: &str,
) -> Result<PathBuf> {
    let dir = config.prompts_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create prompts dir at {}", dir.display()))?;
    let path = dir.join(format!("{project}-{sanitized}.md"));
    std::fs::write(&path, prompt)
        .with_context(|| format!("failed to write prompt file at {}", path.display()))?;
    Ok(path)
}

/// Build the status-reporting preamble every line cook is handed, with the
/// actual project/branch substituted so the command is copy-pasteable.
fn status_protocol_preamble(project: &str, branch: &str) -> String {
    format!(
        "## Reporting your status\n\
         Report your task status to the head chef by running (from any dir):\n\
        \x20   nix run ~/.yeschef/yeschef-src -- ticket {project} {branch} status-set <STATUS>\n\
         Set IN_PROGRESS when you start real work, BLOCKED if you're stuck and need a\n\
         decision, and DONE when the work is finished and the PR is open. Do this\n\
         proactively as your state changes."
    )
}

/// Resolve a registered ticket, returning a clear error if it doesn't exist.
fn require_ticket(config: &Config, project: &str, branch: &str) -> Result<TicketRow> {
    if !config.store.project_exists(project)? {
        bail!("project '{project}' not found; run 'yeschef project add <git-url>' first");
    }
    config.store.lookup_ticket(project, branch)?.ok_or_else(|| {
        anyhow!("no ticket for '{project}/{branch}'; run 'yeschef spawn {project} {branch}' first")
    })
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

pub fn run_spawn(
    config: &Config,
    project: &str,
    branch: &str,
    base: Option<&str>,
    agent: &str,
    prompt: Option<&str>,
) -> Result<()> {
    if !config.store.project_exists(project)? {
        bail!("project '{project}' not found; run 'yeschef project add <git-url>' first");
    }

    let sanitized = sanitize_branch(branch);
    let session = yeschef_session();
    let window = window_name(project, &sanitized);
    let bare_dir = config.bare_repo_dir(project);
    let worktree_path = config.worktree_dir(project, branch);

    // Refuse to clobber a ticket that's already running in a live window.
    config.zmx.ensure_session(session)?;
    if config.zmx.window_exists(session, &window)? {
        bail!(
            "a window for '{project}/{branch}' already exists; use 'yeschef send/peek {project} {branch}' or 'yeschef kill {project} {branch}' first"
        );
    }

    let mut guard = RollbackGuard::new();

    // 1. Create the worktree if it doesn't already exist.
    if worktree_path.exists() {
        eprintln!("[spawn] reusing worktree at {}", worktree_path.display());
    } else {
        let base_branch = match base {
            Some(b) => b.to_string(),
            None => config
                .git
                .default_branch(&bare_dir)
                .context("failed to determine default branch")?,
        };
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

        // `git worktree add` with useRelativePaths enabled writes
        // `extensions.relativeWorktrees = true`, which some libgit2 consumers
        // reject. The relative gitdir paths resolve fine without the marker.
        config
            .git
            .unset_config(&bare_dir, "extensions.relativeWorktrees")
            .context("failed to unset extensions.relativeWorktrees on bare repo")?;
    }

    // 2. Launch the agent in a fresh zmx session rooted at the worktree.
    //
    // The prompt is never passed inline: a long prompt (a few paragraphs) blows
    // past the OS arg-length limit and the agent harness, treating the giant
    // positional arg as a path, dies with `ENAMETOOLONG`. Instead we write the
    // prompt to a file outside the worktree and hand the agent a short
    // instruction to read it. This stays agent-agnostic — claude/codex/aider all
    // take an initial instruction as their positional arg.
    //
    // Every line cook is handed the status-reporting protocol, whether or not a
    // `-p` prompt was supplied — so we always write a brief file (preamble, plus
    // the user prompt if any) and always launch via the read-this-file
    // instruction.
    let preamble = status_protocol_preamble(project, branch);
    let brief = match prompt {
        Some(p) => format!("{preamble}\n\n---\n\n{p}"),
        None => preamble,
    };
    let prompt_path = write_prompt_file(config, project, &sanitized, &brief)?;
    let instruction = format!(
        "Read the ticket brief at {} and carry it out start to finish.",
        prompt_path.display()
    );
    let command = format!("{agent} {}", shell_single_quote(&instruction));
    config
        .zmx
        .new_window(session, &window, &worktree_path, &command)
        .with_context(|| format!("failed to create zmx session '{session}-{window}'"))?;

    // 3. Register the ticket.
    config
        .store
        .register_ticket(project, branch, &sanitized, &window, agent)
        .with_context(|| format!("failed to register ticket '{project}/{branch}'"))?;

    guard.commit();

    println!("spawned '{project}/{branch}' → agent '{agent}' in window '{window}'");
    println!("  peek:   yeschef peek {project} {branch}");
    println!("  steer:  yeschef send {project} {branch} \"<instruction>\"");
    Ok(())
}

// ---------------------------------------------------------------------------
// send
// ---------------------------------------------------------------------------

pub fn run_send(config: &Config, project: &str, branch: &str, text: &str) -> Result<()> {
    let ticket = require_ticket(config, project, branch)?;
    config
        .zmx
        .send_keys(yeschef_session(), &ticket.window, text)
        .with_context(|| format!("failed to send keys to '{project}/{branch}'"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// peek
// ---------------------------------------------------------------------------

pub fn run_peek(config: &Config, project: &str, branch: &str, lines: Option<usize>) -> Result<()> {
    let ticket = require_ticket(config, project, branch)?;
    let pane = config
        .zmx
        .capture_pane(
            yeschef_session(),
            &ticket.window,
            Some(lines.unwrap_or(PEEK_LINES)),
        )
        .with_context(|| format!("failed to capture pane for '{project}/{branch}'"))?;
    print!("{pane}");
    if !pane.ends_with('\n') {
        println!();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

pub fn run_status(config: &Config) -> Result<()> {
    let tickets = config.store.list_tickets()?;
    if tickets.is_empty() {
        println!("no active tickets; run 'yeschef spawn <project> <branch>' to start one");
        return Ok(());
    }

    let session = yeschef_session();
    let windows = config.zmx.list_windows(session).unwrap_or_default();

    println!(
        "{:<28} {:<10} {:<12} {:<12} LAST LINE",
        "TICKET", "AGENT", "STATE", "STATUS"
    );
    for ticket in &tickets {
        let info = windows.iter().find(|w| w.name == ticket.window);
        let state = match info {
            Some(w) if w.dead => "dead",
            Some(_) => "running",
            None => "gone",
        };
        let last_line = if matches!(state, "running") {
            config
                .zmx
                .capture_pane(session, &ticket.window, Some(5))
                .ok()
                .and_then(|p| {
                    p.lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .map(str::to_string)
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        let label = format!("{}/{}", ticket.project, ticket.branch);
        println!(
            "{label:<28} {:<10} {state:<12} {:<12} {last_line}",
            ticket.agent, ticket.status
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ticket status-set
// ---------------------------------------------------------------------------

pub fn run_ticket_status_set(
    config: &Config,
    project: &str,
    branch: &str,
    status: TaskStatus,
) -> Result<()> {
    // Ensure the ticket exists (clear error otherwise) before writing.
    require_ticket(config, project, branch)?;
    config
        .store
        .set_ticket_status(project, branch, status.as_str())
        .with_context(|| format!("failed to set status for '{project}/{branch}'"))?;
    println!("status of '{project}/{branch}' set to {}", status.as_str());
    Ok(())
}

// ---------------------------------------------------------------------------
// kill
// ---------------------------------------------------------------------------

pub fn run_kill(config: &Config, project: &str, branch: &str, rm_worktree: bool) -> Result<()> {
    let ticket = require_ticket(config, project, branch)?;
    let session = yeschef_session();

    config
        .zmx
        .kill_window(session, &ticket.window)
        .with_context(|| format!("failed to kill window for '{project}/{branch}'"))?;

    if rm_worktree {
        let bare_dir = config.bare_repo_dir(project);
        let worktree_path = config.worktree_dir(project, branch);
        config
            .git
            .remove_worktree(&bare_dir, &worktree_path)
            .with_context(|| format!("failed to remove worktree for '{project}/{branch}'"))?;
    }

    config
        .store
        .remove_ticket(project, branch)
        .with_context(|| format!("failed to deregister ticket '{project}/{branch}'"))?;

    if rm_worktree {
        println!("killed '{project}/{branch}' and removed its worktree");
    } else {
        println!("killed '{project}/{branch}' (worktree kept; re-spawn to resume)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// attach
// ---------------------------------------------------------------------------

pub fn run_attach(config: &Config, project: Option<&str>, branch: Option<&str>) -> Result<()> {
    let session = yeschef_session();
    if !config.zmx.session_exists(session)? {
        bail!(
            "no yeschef session yet; spawn a ticket first with 'yeschef spawn <project> <branch>'"
        );
    }

    let window = match (project, branch) {
        (Some(p), Some(b)) => Some(require_ticket(config, p, b)?.window),
        _ => None,
    };

    config
        .zmx
        .attach(session, window.as_deref())
        .context("failed to attach to yeschef session")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::{MockGitBackend, MockZmxBackend};
    use crate::store::Store;
    use tempfile::TempDir;

    /// A Config backed by mocks + an in-memory store, with one project already
    /// registered. The mocks are `Arc`-backed and `Clone`, so the returned
    /// handles share state with the copies inside `config` — inspect calls
    /// through them. Keep `_tmp` alive for the duration of the test.
    struct Harness {
        config: Config,
        zmx: MockZmxBackend,
        git: MockGitBackend,
        _tmp: TempDir,
    }

    fn harness(zmx: MockZmxBackend) -> Harness {
        let tmp = TempDir::new().unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .add_project("proj", "https://example.com/proj.git")
            .unwrap();
        let git = MockGitBackend::new();
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

    #[test]
    fn shell_single_quote_escapes_quotes() {
        assert_eq!(shell_single_quote("hi"), "'hi'");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn spawn_creates_window_and_registers_ticket() {
        let h = harness(MockZmxBackend::new());
        run_spawn(
            &h.config,
            "proj",
            "feature/x",
            None,
            "claude",
            Some("do it"),
        )
        .unwrap();

        // Ticket is registered with the derived window name.
        let ticket = h
            .config
            .store
            .lookup_ticket("proj", "feature/x")
            .unwrap()
            .unwrap();
        assert_eq!(ticket.window, "proj-feature-x");
        assert_eq!(ticket.agent, "claude");

        // The window launches the agent with a short "read this file"
        // instruction rather than the prompt inlined on the command line.
        let calls = h.zmx.recorded_calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("new_window:yeschef:proj-feature-x:")
                    && c.contains("claude 'Read the ticket brief at ")
                    && c.contains("carry it out start to finish.'")),
            "calls: {calls:?}"
        );
    }

    #[test]
    fn spawn_writes_long_prompt_to_file_not_inline() {
        let h = harness(MockZmxBackend::new());
        // A multi-paragraph prompt that would overflow the arg-length limit if
        // passed inline (the ENAMETOOLONG bug this guards against).
        let long_prompt = "Refactor the widget subsystem.\n\n".repeat(500);
        run_spawn(
            &h.config,
            "proj",
            "feature/x",
            None,
            "claude",
            Some(&long_prompt),
        )
        .unwrap();

        let calls = h.zmx.recorded_calls();
        let launch = calls
            .iter()
            .find(|c| c.starts_with("new_window:yeschef:proj-feature-x:"))
            .expect("expected a new_window call");

        // The raw prompt must NOT appear inline on the launched command.
        assert!(
            !launch.contains("Refactor the widget subsystem"),
            "prompt leaked onto the command line: {launch}"
        );

        // The command references the prompt file by absolute path.
        let prompt_path = h.config.prompts_dir().join("proj-feature-x.md");
        assert!(
            launch.contains(&prompt_path.display().to_string()),
            "command does not reference the prompt file: {launch}"
        );

        // The file lives outside the worktree and holds the full prompt verbatim.
        assert!(
            !prompt_path.starts_with(h.config.worktree_dir("proj", "feature/x")),
            "prompt file must live outside the worktree: {}",
            prompt_path.display()
        );
        // The full prompt is present in the file verbatim (now preceded by the
        // status-protocol preamble, separated by a horizontal rule).
        let written = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            written.contains(&long_prompt),
            "prompt file missing the user prompt"
        );
        assert!(
            written.contains("## Reporting your status"),
            "prompt file missing the status protocol preamble"
        );
        assert!(
            written.contains("\n\n---\n\n"),
            "preamble and user prompt should be separated by a rule"
        );
    }

    #[test]
    fn spawn_injects_status_protocol_even_without_prompt() {
        let h = harness(MockZmxBackend::new());
        run_spawn(&h.config, "proj", "feature/x", None, "claude", None).unwrap();

        // Even with no -p prompt, a brief file is written carrying the protocol,
        // and the agent is launched via the read-this-file instruction.
        let prompt_path = h.config.prompts_dir().join("proj-feature-x.md");
        let written = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(written.contains("## Reporting your status"));
        // The command is substituted with the real project/branch.
        assert!(written.contains("ticket proj feature/x status-set <STATUS>"));

        let calls = h.zmx.recorded_calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("new_window:yeschef:proj-feature-x:")
                    && c.contains("claude 'Read the ticket brief at ")),
            "calls: {calls:?}"
        );
    }

    #[test]
    fn ticket_status_set_persists_and_requires_ticket() {
        let h = harness(MockZmxBackend::new());
        run_spawn(&h.config, "proj", "x", None, "claude", None).unwrap();

        run_ticket_status_set(&h.config, "proj", "x", TaskStatus::Blocked).unwrap();
        let ticket = h.config.store.lookup_ticket("proj", "x").unwrap().unwrap();
        assert_eq!(ticket.status, "BLOCKED");

        // Unknown ticket errors, doesn't create a row.
        let err = run_ticket_status_set(&h.config, "proj", "ghost", TaskStatus::Done).unwrap_err();
        assert!(err.to_string().contains("no ticket"), "{err}");
    }

    #[test]
    fn spawn_refuses_duplicate_window() {
        let h = harness(MockZmxBackend::new());
        run_spawn(&h.config, "proj", "x", None, "claude", None).unwrap();
        let err = run_spawn(&h.config, "proj", "x", None, "claude", None).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn spawn_unknown_project_errors() {
        let h = harness(MockZmxBackend::new());
        let err = run_spawn(&h.config, "nope", "x", None, "claude", None).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn send_targets_the_ticket_window() {
        let h = harness(MockZmxBackend::new());
        run_spawn(&h.config, "proj", "x", None, "claude", None).unwrap();
        run_send(&h.config, "proj", "x", "run tests").unwrap();
        let calls = h.zmx.recorded_calls();
        assert!(
            calls.contains(&"send_keys:yeschef:proj-x:run tests".to_string()),
            "calls: {calls:?}"
        );
    }

    #[test]
    fn send_unknown_ticket_errors() {
        let h = harness(MockZmxBackend::new());
        let err = run_send(&h.config, "proj", "ghost", "hi").unwrap_err();
        assert!(err.to_string().contains("no ticket"), "{err}");
    }

    #[test]
    fn peek_returns_pane_content() {
        let zmx = MockZmxBackend::new().with_pane("yeschef", "proj-x", "hello from agent\n");
        let h = harness(zmx);
        run_spawn(&h.config, "proj", "x", None, "claude", None).unwrap();
        run_peek(&h.config, "proj", "x", Some(10)).unwrap();
        let calls = h.zmx.recorded_calls();
        assert!(
            calls.iter().any(|c| c == "capture_pane:yeschef:proj-x:10"),
            "calls: {calls:?}"
        );
    }

    #[test]
    fn kill_removes_window_and_ticket() {
        let h = harness(MockZmxBackend::new());
        run_spawn(&h.config, "proj", "x", None, "claude", None).unwrap();
        run_kill(&h.config, "proj", "x", false).unwrap();
        assert!(h.config.store.lookup_ticket("proj", "x").unwrap().is_none());
        assert!(h
            .zmx
            .recorded_calls()
            .contains(&"kill_window:yeschef:proj-x".to_string()));
        // Without --rm-worktree, the worktree is not removed.
        assert!(!h
            .git
            .recorded_calls()
            .iter()
            .any(|c| c.starts_with("remove_worktree")));
    }

    #[test]
    fn kill_with_rm_worktree_removes_worktree() {
        let h = harness(MockZmxBackend::new());
        run_spawn(&h.config, "proj", "x", None, "claude", None).unwrap();
        run_kill(&h.config, "proj", "x", true).unwrap();
        assert!(h
            .git
            .recorded_calls()
            .iter()
            .any(|c| c.starts_with("remove_worktree")));
    }
}
