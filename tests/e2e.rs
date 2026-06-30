//! End-to-end tests for yeschef.
//!
//! All tests are `#[ignore]` by default. Run with:
//!   cargo test --test e2e -- --ignored --test-threads=1
//!
//! Requirements: `git` and `zmx` in PATH. No containers, no Nix, no macOS
//! requirement — the head chef drives real git worktrees and a real zmx
//! session. `--test-threads=1` keeps the shared `yeschef` zmx sessions sane
//! across tests (each test uses a unique project name, so windows don't clash).

use std::path::Path;
use std::process::Command;

use predicates::prelude::*;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A sandboxed `YESCHEF_HOME` backed by a temp directory.
struct TestEnv {
    _tmp: TempDir,
    home: std::path::PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let tmp = TempDir::new().expect("create temp dir");
        let home = tmp.path().join("yeschef-home");
        TestEnv { _tmp: tmp, home }
    }

    fn home_path(&self) -> &Path {
        &self.home
    }

    fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("yeschef").unwrap();
        cmd.env("YESCHEF_HOME", &self.home);
        cmd
    }

    fn init(&self) {
        self.cmd().arg("init").assert().success();
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

/// The flat zmx session id the backend uses for a ticket window:
/// `<yeschef_session>-<window>`.
fn zid(window: &str) -> String {
    format!("yeschef-{window}")
}

/// Kill a ticket's zmx session on drop (best-effort teardown).
struct WindowCleanup(Vec<String>);

impl Drop for WindowCleanup {
    fn drop(&mut self) {
        for window in &self.0 {
            let _ = Command::new("zmx")
                .args(["kill", &zid(window), "--force"])
                .output();
        }
    }
}

/// Capture a window's scrollback via the real zmx.
fn capture(window: &str) -> String {
    Command::new("zmx")
        .args(["history", &zid(window)])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn window_exists(window: &str) -> bool {
    let id = zid(window);
    Command::new("zmx")
        .args(["ls", "--short"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == id)
        })
}

// ---------------------------------------------------------------------------
// init tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + zmx; use `cargo test --test e2e -- --ignored`"]
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
#[ignore = "requires git + zmx"]
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
#[ignore = "requires git + zmx"]
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
#[ignore = "requires git + zmx"]
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

#[test]
#[ignore = "requires git + zmx"]
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
#[ignore = "requires git + zmx"]
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
// orchestration error tests (no zmx session required)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + zmx"]
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
#[ignore = "requires git + zmx"]
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
#[ignore = "requires git + zmx"]
fn spawn_creates_worktree_and_live_window() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let window = format!("{name}-demo");
    let _cleanup = WindowCleanup(vec![window.clone()]);

    // The prompt is no longer passed inline; spawn writes it to a file and
    // hands the agent a short "read this file" instruction (guards against the
    // ENAMETOOLONG bug on long prompts). The stand-in agent is a shell program
    // that echoes a liveness marker, prints the instruction it received (which
    // arrives as `$0`), and stays alive — mirroring how a real agent takes the
    // instruction as its first positional arg.
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
    // worktree, so it can't be committed) and holds the full prompt verbatim.
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
    assert_eq!(written, prompt_body);

    // zmx session is live.
    assert!(
        window_exists(&window),
        "zmx session for '{window}' should exist"
    );

    // Give the shell a moment to echo, then peek should show the marker and the
    // file-indirection instruction the agent was launched with.
    std::thread::sleep(std::time::Duration::from_millis(800));
    let pane = capture(&window);
    assert!(
        pane.contains("SPAWN_OK"),
        "pane should show the agent's output; got:\n{pane}"
    );
    assert!(
        pane.contains("Read the ticket brief at"),
        "pane should show the read-this-file instruction; got:\n{pane}"
    );

    // status lists the ticket as running.
    env.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}/demo")))
        .stdout(predicate::str::contains("running"));

    // peek via the CLI returns content too.
    env.cmd()
        .args(["peek", &name, "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SPAWN_OK"));
}

#[test]
#[ignore = "requires git + zmx"]
fn send_reaches_the_pane() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let window = format!("{name}-demo");
    let _cleanup = WindowCleanup(vec![window.clone()]);

    // A `cat` loop echoes whatever we send into the pane.
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "cat"])
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_millis(500));

    env.cmd()
        .args(["send", &name, "demo", "HELLO_FROM_SEND"])
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_millis(500));

    let pane = capture(&window);
    assert!(
        pane.contains("HELLO_FROM_SEND"),
        "sent text should appear in the pane; got:\n{pane}"
    );
}

#[test]
#[ignore = "requires git + zmx"]
fn kill_removes_window_and_deregisters() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let window = format!("{name}-demo");
    let _cleanup = WindowCleanup(vec![window.clone()]);

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c", "-p", "sleep 30"])
        .assert()
        .success();
    assert!(window_exists(&window), "window should exist after spawn");

    env.cmd()
        .args(["kill", &name, "demo", "--rm-worktree"])
        .assert()
        .success();

    assert!(!window_exists(&window), "window should be gone after kill");

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

#[test]
#[ignore = "requires git + zmx"]
fn spawn_duplicate_window_rejected() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let window = format!("{name}-demo");
    let _cleanup = WindowCleanup(vec![window]);

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c", "-p", "sleep 30"])
        .assert()
        .success();
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c", "-p", "sleep 30"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}
