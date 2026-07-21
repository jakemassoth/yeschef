//! End-to-end tests for yeschef.
//!
//! All tests are `#[ignore]` by default. Run with:
//!   cargo test --test e2e -- --ignored
//!
//! Requirements: `git` and `herdr` in PATH. The tests drive real git worktrees
//! and a real herdr server.
//!
//! ## Session isolation
//!
//! The suite NEVER touches the operator's live `yeschef` herdr brigade. Each
//! [`TestEnv`] uses its OWN unique, throwaway herdr session name (see
//! [`unique_session`]), exported as `YESCHEF_HERDR_SESSION` so every `yeschef`
//! binary that env spawns — and the detached herdr server it starts — drives
//! that one private session. Each env also gets its OWN `XDG_CONFIG_HOME` (herdr
//! derives its socket path, session state, and logs from there), rooted under
//! the env's temp dir, so two tests never share a herdr server or its on-disk
//! state. On `Drop` the env stops its server (`herdr server stop`) and the temp
//! dir removes the config home with it. Because every test has a fully
//! independent session + config home, the suite is safe under cargo's default
//! parallel execution — no `--test-threads=1` needed.
//!
//! The temp dir is rooted at `/tmp` (short) on purpose: herdr puts its API
//! socket at `$XDG_CONFIG_HOME/herdr/sessions/<session>/herdr.sock`, and a unix
//! socket path has a hard ~104-byte limit (`sun_path`). macOS's default temp dir
//! (`/var/folders/…`) is deep enough to overflow it; `/tmp` keeps the path short.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use predicates::prelude::*;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A sandboxed `YESCHEF_HOME` backed by a temp directory, wired to its OWN
/// private throwaway herdr session + `XDG_CONFIG_HOME`. Two tests — even running
/// in parallel — never share a herdr server, and `Drop` tears the server down
/// automatically.
struct TestEnv {
    /// Held only to keep the temp dir alive for the env's lifetime; its `Drop`
    /// removes the config home (and everything else) after the env's own `Drop`.
    _tmp: TempDir,
    home: std::path::PathBuf,
    /// This env's private herdr `XDG_CONFIG_HOME` (herdr's socket + session
    /// state + logs live here). Under `tmp`, so it's removed on drop.
    config_home: std::path::PathBuf,
    /// This env's private herdr session name, handed to every spawned `yeschef`
    /// binary via `YESCHEF_HERDR_SESSION` and to the direct `herdr` helpers.
    session: String,
}

impl TestEnv {
    fn new() -> Self {
        // See the module docs for why the temp dir is rooted at `/tmp` (herdr
        // socket path length under `sun_path`'s ~104-byte limit).
        let tmp = TempDir::new_in("/tmp").expect("create temp dir under /tmp");
        let home = tmp.path().join("yeschef-home");
        let config_home = tmp.path().join("cfg");
        TestEnv {
            _tmp: tmp,
            home,
            config_home,
            session: unique_session(),
        }
    }

    fn home_path(&self) -> &Path {
        &self.home
    }

    fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("yeschef").unwrap();
        cmd.env("YESCHEF_HOME", &self.home);
        // Pin the spawned binary (and the detached herdr server it starts) to
        // THIS env's private herdr session + config home: a unique session name
        // and an `XDG_CONFIG_HOME` under this env's temp dir. Nothing ever drives
        // the operator's live `yeschef` brigade or another test's server, and the
        // config home is cleaned up with the temp dir on drop.
        cmd.env("YESCHEF_HERDR_SESSION", &self.session);
        cmd.env("XDG_CONFIG_HOME", &self.config_home);
        // The brigade pins a head chef. Point it at a long-lived stand-in (not
        // the real `claude`, absent in CI) rooted in a directory that exists.
        cmd.env("YESCHEF_SRC", &self.home);
        cmd.env("YESCHEF_HEADCHEF_CMD", "sh -c 'exec sleep 300'");
        cmd
    }

    fn init(&self) {
        self.cmd().arg("init").assert().success();
    }

    /// A `herdr` command wired to this env's private session + config home.
    fn herdr(&self, args: &[&str]) -> std::process::Output {
        Command::new("herdr")
            .env("XDG_CONFIG_HOME", &self.config_home)
            .args(["--session", &self.session])
            .args(args)
            .output()
            .expect("failed to run herdr")
    }

    /// Raw `herdr workspace list` stdout (empty string if the server is down).
    fn workspace_list(&self) -> String {
        let out = self.herdr(&["workspace", "list"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Whether a workspace with the given label exists in the brigade session.
    fn has_workspace(&self, label: &str) -> bool {
        self.workspace_list()
            .contains(&format!("\"label\":\"{label}\""))
    }

    /// Whether this env's herdr server is currently running.
    fn server_running(&self) -> bool {
        let out = self.herdr(&["status", "server"]);
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("status: running")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Stop this env's herdr server promptly — on pass, fail, or panic — so no
        // detached throwaway server leaks. Best-effort (the server may never have
        // started). The on-disk state (socket, session json, logs) lives under
        // `self.tmp`'s config home, removed when `tmp` drops right after.
        let _ = Command::new("herdr")
            .env("XDG_CONFIG_HOME", &self.config_home)
            .args(["--session", &self.session, "server", "stop"])
            .output();
    }
}

/// A temporary local git repository to clone from.
struct SampleRepo {
    dir: TempDir,
}

impl SampleRepo {
    fn new() -> Self {
        let dir = TempDir::new().expect("create sample repo dir");
        let path = dir.path();

        std::fs::write(path.join("README.md"), "# sample\n").unwrap();

        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "test@yeschef.test"]);
        git(path, &["config", "user.name", "Test"]);
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "initial"]);

        SampleRepo { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn url(&self) -> String {
        format!("file://{}", self.path().display())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|_| panic!("git {args:?} failed to spawn"));
    assert!(status.success(), "git {args:?} exited non-zero");
}

/// Unique project name: lowercase alphanumeric, safe for yeschef validation.
fn unique_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let pid = std::process::id();
    format!("t{pid:08x}{:04x}", nanos & 0xffff)
}

/// A globally-unique herdr session name for one [`TestEnv`]. Combines the PID, a
/// nanosecond timestamp, and a process-wide atomic counter so no two envs ever
/// collide: not two created back-to-back in the same thread (the counter breaks
/// any timestamp tie), not two separate test-binary runs (distinct PIDs), and
/// not a run beside a live yeschef instance (the `yt-` prefix differs from the
/// production `yeschef` session). Kept short so that, combined with the config
/// home under `/tmp`, herdr's `sun_path` socket stays under its ~104-byte limit.
fn unique_session() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("yt-{pid:x}-{:x}-{seq:x}", nanos & 0xffff_ffff)
}

// ---------------------------------------------------------------------------
// init tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + herdr; use `cargo test --test e2e -- --ignored`"]
fn init_creates_expected_layout() {
    let env = TestEnv::new();

    env.cmd()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized"));

    assert!(env.home_path().join("projects").is_dir(), "projects/ dir");
    assert!(env.home_path().join("yeschef.db").is_file(), "yeschef.db");
    assert!(env.home_path().join("AGENTS.md").is_file(), "AGENTS.md");
}

#[test]
#[ignore = "requires git + herdr"]
fn init_is_idempotent() {
    let env = TestEnv::new();
    env.cmd().arg("init").assert().success();
    env.cmd()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("already exists"));
}

// ---------------------------------------------------------------------------
// project tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + herdr"]
fn project_list_empty() {
    let env = TestEnv::new();
    env.init();
    env.cmd()
        .args(["project", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no projects"));
}

#[test]
#[ignore = "requires git + herdr"]
fn project_add_registers_bare_clone() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();

    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success()
        .stdout(predicate::str::contains("added"));

    let bare = env.home_path().join("projects").join(&name).join(".bare");
    assert!(
        bare.is_dir(),
        "bare repo should exist at {}",
        bare.display()
    );

    env.cmd()
        .args(["project", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&name));
}

/// Run `git` in a bare repo and return trimmed stdout, asserting success.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|_| panic!("git {args:?} failed to spawn"));
    assert!(
        out.status.success(),
        "git {args:?} exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
#[ignore = "requires git + herdr"]
fn project_add_makes_origin_main_resolve() {
    // The core fix: after `project add`, `origin/main` must resolve in the bare
    // clone so `spawn --base origin/main` and `git rebase origin/main` work.
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();

    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let bare = env.home_path().join("projects").join(&name).join(".bare");
    // origin/main resolves to the same commit as the local main head.
    let origin_main = git_out(&bare, &["rev-parse", "origin/main"]);
    let head_main = git_out(&bare, &["rev-parse", "main"]);
    assert_eq!(
        origin_main, head_main,
        "origin/main should resolve to the fetched tip"
    );
    let refs = git_out(&bare, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.contains("refs/remotes/origin/main"),
        "expected remote-tracking ref, got: {refs}"
    );
}

#[test]
#[ignore = "requires git + herdr"]
fn refresh_repairs_clone_with_no_tracking_refspec() {
    // Migration path: a bare clone created the old way (no fetch refspec) has no
    // origin/* refs. `refresh` must repair the refspec and populate them.
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();

    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();
    let bare = env.home_path().join("projects").join(&name).join(".bare");
    // Tear the clone back down to the broken pre-fix state.
    git(&bare, &["config", "--unset-all", "remote.origin.fetch"]);
    git(&bare, &["update-ref", "-d", "refs/remotes/origin/main"]);
    git(&bare, &["update-ref", "-d", "refs/remotes/origin/HEAD"]);
    assert!(
        !Command::new("git")
            .args(["rev-parse", "origin/main"])
            .current_dir(&bare)
            .output()
            .unwrap()
            .status
            .success(),
        "precondition: origin/main should NOT resolve before refresh"
    );

    env.cmd().args(["refresh", &name]).assert().success();

    let origin_main = git_out(&bare, &["rev-parse", "origin/main"]);
    let head_main = git_out(&bare, &["rev-parse", "main"]);
    assert_eq!(
        origin_main, head_main,
        "refresh should repair the refspec and populate origin/main"
    );
}

#[test]
#[ignore = "requires git + herdr"]
fn project_add_duplicate_name_rejected() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();

    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
#[ignore = "requires git + herdr"]
fn project_add_invalid_name_rejected() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    for bad in &["Bad Name", "UPPER", "foo/bar", "-leading", "trailing-"] {
        env.cmd()
            .args(["project", "add", &repo.url(), bad])
            .assert()
            .failure();
    }
}

// ---------------------------------------------------------------------------
// orchestration error tests (no herdr server required)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + herdr"]
fn spawn_unknown_project_gives_clear_error() {
    let env = TestEnv::new();
    env.init();
    env.cmd()
        .args(["spawn", "nonexistent-proj", "main"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
#[ignore = "requires git + herdr"]
fn send_unknown_ticket_gives_clear_error() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();
    env.cmd()
        .args(["send", &name, "no-such-branch", "hi"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no ticket"));
}

// ---------------------------------------------------------------------------
// full lifecycle: spawn → peek → send → status → kill
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + herdr"]
fn spawn_creates_worktree_and_live_workspace() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let label = format!("{name}/demo");

    // The prompt is written to a file; the agent is launched with a short "read
    // this file" instruction (guards against the ENAMETOOLONG bug on long
    // prompts). The stand-in agent echoes a liveness marker, prints the
    // instruction it received (which arrives as `$0`), and stays alive.
    let prompt_body = "PROMPT_BODY_MARKER: refactor the widget subsystem.";
    env.cmd()
        .args([
            "spawn",
            &name,
            "demo",
            "--agent",
            "sh -c 'echo SPAWN_OK; echo \"$0\"; sleep 30'",
            "-p",
            prompt_body,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("spawned"));

    // Worktree exists on disk.
    let worktree = env
        .home_path()
        .join("projects")
        .join(&name)
        .join("worktrees")
        .join("demo");
    assert!(
        worktree.is_dir(),
        "worktree should exist at {}",
        worktree.display()
    );

    // The prompt was written to a file under the yeschef home (outside the
    // worktree, so it can't be committed) and holds the full prompt verbatim,
    // preceded by the status-reporting protocol preamble.
    let prompt_file = env
        .home_path()
        .join("prompts")
        .join(format!("{name}-demo.md"));
    assert!(
        prompt_file.is_file(),
        "prompt file should exist at {}",
        prompt_file.display()
    );
    assert!(
        !prompt_file.starts_with(&worktree),
        "prompt file must live outside the worktree"
    );
    let written = std::fs::read_to_string(&prompt_file).unwrap();
    assert!(
        written.contains("## Reporting your status"),
        "prompt file should carry the status protocol preamble; got:\n{written}"
    );
    assert!(
        written.ends_with(&format!("---\n\n{prompt_body}")),
        "prompt file should end with the verbatim user prompt after the rule; got:\n{written}"
    );

    // The herdr workspace is live.
    assert!(
        env.has_workspace(&label),
        "herdr workspace '{label}' should exist; got:\n{}",
        env.workspace_list()
    );

    // Give the shell a moment to echo, then peek should show the marker and the
    // file-indirection instruction the agent was launched with.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    env.cmd()
        .args(["peek", &name, "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SPAWN_OK"))
        .stdout(predicate::str::contains("Read the ticket brief at"));

    // status lists the ticket (not gone).
    env.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}/demo")))
        .stdout(predicate::str::contains("gone").not());
}

#[test]
#[ignore = "requires git + herdr"]
fn send_reaches_the_pane() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    // `cat` reading stdin echoes whatever we send into the pane and stays alive.
    // Wrapping it in `sh -c 'cat'` keeps cat argument-free — the read-this-file
    // instruction lands in `$0` — so it reads stdin instead of treating the
    // instruction as a filename and exiting.
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'cat'"])
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_secs(1));

    env.cmd()
        .args(["send", &name, "demo", "HELLO_FROM_SEND"])
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_millis(800));

    env.cmd()
        .args(["peek", &name, "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("HELLO_FROM_SEND"));
}

#[test]
#[ignore = "requires git + herdr"]
fn kill_removes_workspace_and_deregisters() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let label = format!("{name}/demo");

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.has_workspace(&label),
        "workspace should exist after spawn"
    );

    env.cmd()
        .args(["kill", &name, "demo", "--rm-worktree"])
        .assert()
        .success();

    assert!(
        !env.has_workspace(&label),
        "workspace should be gone after kill; got:\n{}",
        env.workspace_list()
    );

    let worktree = env
        .home_path()
        .join("projects")
        .join(&name)
        .join("worktrees")
        .join("demo");
    assert!(
        !worktree.exists(),
        "worktree should be removed with --rm-worktree"
    );

    // Ticket no longer listed.
    env.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}/demo")).not());
}

// ---------------------------------------------------------------------------
// cleanup
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + herdr"]
fn cleanup_dry_run_reports_without_removing() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let label = format!("{name}/demo");

    // A branch spawned from `main` points at main's tip, so it is an ancestor
    // of origin/main — classified "merged". That alone is no longer enough to
    // reap: cleanup also requires the cook to be inactive. Report DONE so the
    // ticket becomes a genuine reap candidate for the dry run to spare.
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.has_workspace(&label),
        "workspace should exist after spawn"
    );
    env.cmd()
        .args(["ticket", &name, "demo", "status-set", "DONE"])
        .assert()
        .success();

    // Default cleanup (no --yes) is a dry run: it reports the candidate but
    // removes nothing.
    env.cmd()
        .args(["cleanup", &name])
        .assert()
        .success()
        .stdout(predicate::str::contains("would reap"))
        .stdout(predicate::str::contains(format!("{name}/demo")));

    // The workspace, worktree, and registry entry all survive the dry run.
    assert!(
        env.has_workspace(&label),
        "dry run must not close the workspace"
    );
    let worktree = env
        .home_path()
        .join("projects")
        .join(&name)
        .join("worktrees")
        .join("demo");
    assert!(worktree.is_dir(), "dry run must not remove the worktree");
    env.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}/demo")));
}

#[test]
#[ignore = "requires git + herdr"]
fn cleanup_yes_reaps_merged_ticket() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let label = format!("{name}/demo");

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.has_workspace(&label),
        "workspace should exist after spawn"
    );

    // The branch is "merged" (points at main's tip), but reaping also requires
    // the cook to be inactive. Report DONE so both gates agree.
    env.cmd()
        .args(["ticket", &name, "demo", "status-set", "DONE"])
        .assert()
        .success();

    env.cmd()
        .args(["cleanup", &name, "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reaped"))
        .stdout(predicate::str::contains(format!("{name}/demo")));

    // Workspace, worktree, and registry entry are all gone.
    assert!(
        !env.has_workspace(&label),
        "workspace should be closed by cleanup; got:\n{}",
        env.workspace_list()
    );
    let worktree = env
        .home_path()
        .join("projects")
        .join(&name)
        .join("worktrees")
        .join("demo");
    assert!(!worktree.exists(), "worktree should be removed by cleanup");
    env.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}/demo")).not());
}

#[test]
#[ignore = "requires git + herdr"]
fn cleanup_yes_keeps_active_ticket_even_when_merged() {
    // Regression guard: a freshly-spawned ticket has no commits, so its branch
    // classifies "merged" — but the cook is still working it (status stays at
    // the NEW default). `cleanup --yes` must NOT reap live work.
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let label = format!("{name}/demo");

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.has_workspace(&label),
        "workspace should exist after spawn"
    );

    // Even with --yes, the merged-but-active ticket is kept, and the report says
    // why. (The summary line always contains the word "reaped" — e.g. "0 reaped,
    // 1 kept" — so we assert on the absence of the per-ticket reap line.)
    env.cmd()
        .args(["cleanup", &name, "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("keep   {name}/demo")))
        .stdout(predicate::str::contains("active"))
        .stdout(predicate::str::contains(format!("reaped {name}/demo")).not());

    // Workspace, worktree, and registry entry all survive.
    assert!(
        env.has_workspace(&label),
        "active ticket's workspace must survive"
    );
    let worktree = env
        .home_path()
        .join("projects")
        .join(&name)
        .join("worktrees")
        .join("demo");
    assert!(worktree.is_dir(), "active ticket's worktree must survive");
    env.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}/demo")));
}

// ---------------------------------------------------------------------------
// restart
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + herdr"]
fn restart_restores_the_brigade() {
    // `restart` stops and restarts the herdr server. herdr persists the session
    // to disk, so the workspaces come back after the bounce (and, for supported
    // integrations, agents resume — our stand-in isn't one, so we only assert
    // the workspace is restored, not the process).
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let label = format!("{name}/demo");
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.has_workspace(&label),
        "workspace should exist after spawn"
    );

    env.cmd()
        .args(["restart"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restarted"));

    std::thread::sleep(std::time::Duration::from_millis(800));

    // The server is back up and the cook's workspace was restored from herdr's
    // persisted session state.
    assert!(
        env.server_running(),
        "the brigade server must be running again after restart"
    );
    assert!(
        env.has_workspace(&label),
        "the cook's workspace must be restored after restart; got:\n{}",
        env.workspace_list()
    );
}

#[test]
#[ignore = "requires git + herdr"]
fn restart_without_server_errors() {
    // With no brigade server up, there's nothing running to restart — say so
    // rather than silently doing nothing.
    let env = TestEnv::new();
    env.init();
    env.cmd()
        .args(["restart"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to restart"));
}

#[test]
#[ignore = "requires git + herdr"]
fn spawn_duplicate_workspace_rejected() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}
