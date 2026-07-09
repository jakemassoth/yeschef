//! End-to-end tests for yeschef.
//!
//! All tests are `#[ignore]` by default. Run with:
//!   cargo test --test e2e -- --ignored
//!
//! Requirements: `git` and `tmux` in PATH. No containers, no Nix, no macOS
//! requirement — the tests drive real git worktrees and real tmux sessions.
//!
//! ## Socket isolation
//!
//! The suite NEVER touches the operator's live `yeschef` tmux server. Each
//! [`TestEnv`] mints its OWN unique, throwaway `-L` socket (see
//! [`unique_socket`]), exports it as `YESCHEF_TMUX_SOCKET` so every `yeschef`
//! binary that env spawns drives that same private server, and points every
//! direct `tmux` helper on the env at it too. So a `kill-session` (or the
//! whole-server `kill-server` teardown) can only ever affect that one env's
//! server — never another test's, and never the operator's live session (even
//! when the suite is run from inside one). When a `TestEnv` drops, its `Drop`
//! impl runs `kill-server` to dispose of the private server process on pass,
//! fail, or panic; the socket FILE itself lives under the env's temp dir (via
//! `TMUX_TMPDIR`), so it is removed with that dir too — nothing (server or
//! socket) leaks into the shared `/tmp/tmux-<uid>/` tree. Because every test has
//! a fully independent server, the suite is safe under cargo's default parallel
//! execution — no `--test-threads=1` needed.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use predicates::prelude::*;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A sandboxed `YESCHEF_HOME` backed by a temp directory, wired to its OWN
/// private throwaway tmux server. The socket is unique per env (see
/// [`unique_socket`]), so two tests — even running in parallel — never share a
/// tmux server, and `Drop` tears the server down automatically.
struct TestEnv {
    tmp: TempDir,
    home: std::path::PathBuf,
    /// This env's private tmux `-L` socket name. Handed to every spawned
    /// `yeschef` binary via `YESCHEF_TMUX_SOCKET` and to every direct `tmux`
    /// helper on the env, and torn down in `Drop`.
    socket: String,
}

impl TestEnv {
    fn new() -> Self {
        // Root the temp dir at `/tmp` rather than the platform default. We point
        // `TMUX_TMPDIR` here (see `cmd`) so the tmux socket lives under it and is
        // cleaned up on drop — but a unix socket path has a hard ~104-byte limit
        // (`sun_path`), and macOS's default temp dir (`/var/folders/…`) is deep
        // enough that the socket path overflows it ("File name too long"). `/tmp`
        // is short, present on both macOS and Linux, and is where tmux puts its
        // sockets by default anyway, so the relocated socket path stays valid.
        let tmp = TempDir::new_in("/tmp").expect("create temp dir under /tmp");
        let home = tmp.path().join("yeschef-home");
        TestEnv {
            tmp,
            home,
            socket: unique_socket(),
        }
    }

    fn home_path(&self) -> &Path {
        &self.home
    }

    fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("yeschef").unwrap();
        cmd.env("YESCHEF_HOME", &self.home);
        // Pin the spawned binary to THIS env's private tmux server: a unique
        // `-L` socket name, relocated under this env's temp dir via
        // `TMUX_TMPDIR`. The socket never drives the operator's live `yeschef`
        // server or another test's server, and the relocation means its socket
        // FILE is cleaned up with the temp dir on drop (see `Drop`) rather than
        // littering the shared `/tmp/tmux-<uid>/` tree.
        cmd.env("YESCHEF_TMUX_SOCKET", &self.socket);
        cmd.env("TMUX_TMPDIR", self.tmp.path());
        cmd
    }

    fn init(&self) {
        self.cmd().arg("init").assert().success();
    }

    /// A `tmux` command wired to this env's private server (same `-L` socket +
    /// `TMUX_TMPDIR` as [`TestEnv::cmd`], so they resolve to the same server).
    /// Read helpers don't need `-f` — the server is already running with its
    /// config from the binary's spawn.
    fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", self.tmp.path())
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .expect("failed to run tmux")
    }

    /// Capture a window's scrollback via this env's tmux server.
    fn capture(&self, window: &str) -> String {
        let out = self.tmux(&["capture-pane", "-p", "-S", "-", "-t", &sid(window)]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn window_exists(&self, window: &str) -> bool {
        let id = sid(window);
        let out = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        out.status.success()
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l.trim() == id)
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // RAII teardown, two layers:
        //   1. `kill-server` disposes this env's tmux server PROCESS (and every
        //      session it held) promptly — on pass, fail, or panic — so no live
        //      throwaway server ever leaks. `TMUX_TMPDIR` MUST match `cmd`/`tmux`
        //      so `-L` resolves to the same socket the server actually runs on.
        //   2. The socket FILE (which tmux leaves behind after `kill-server`)
        //      lives under `self.tmp`, so it is removed with the temp dir when
        //      the `tmp` field drops immediately after this — keeping the shared
        //      `/tmp/tmux-<uid>/` tree clean with no socket-path math.
        // Best-effort — the server may already be gone if none was started.
        let _ = Command::new("tmux")
            .env("TMUX_TMPDIR", self.tmp.path())
            .args(["-L", &self.socket, "kill-server"])
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

/// A globally-unique tmux `-L` socket name for one [`TestEnv`]. Combines the
/// PID, a nanosecond timestamp, and a process-wide atomic counter so no two
/// envs ever collide: not two created back-to-back in the same thread (the
/// counter breaks any timestamp tie), not two separate test-binary runs
/// (distinct PIDs), and not a run beside a live yeschef instance (the
/// `yeschef-test-` prefix differs from the production `yeschef` socket). Each
/// env stands up and tears down the server on this socket by itself.
fn unique_socket() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("yeschef-test-{pid:08x}-{nanos:x}-{seq:x}")
}

/// The flat tmux session id the backend uses for a ticket window:
/// `<yeschef_session>-<window>`.
fn sid(window: &str) -> String {
    format!("yeschef-{window}")
}

// ---------------------------------------------------------------------------
// init tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + tmux; use `cargo test --test e2e -- --ignored`"]
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
#[ignore = "requires git + tmux"]
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
#[ignore = "requires git + tmux"]
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
#[ignore = "requires git + tmux"]
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
#[ignore = "requires git + tmux"]
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
#[ignore = "requires git + tmux"]
fn refresh_repairs_clone_with_no_tracking_refspec() {
    // Migration path: a bare clone created the old way (no fetch refspec) has no
    // origin/* refs. `refresh` must repair the refspec and populate them.
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();

    // Simulate a pre-fix registration: clone bare ourselves so no refspec/fetch
    // happens, then register the name in yeschef via a normal add against a
    // throwaway path is not possible — instead, add then strip the refspec to
    // mimic the broken on-disk state of an old clone.
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
#[ignore = "requires git + tmux"]
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
#[ignore = "requires git + tmux"]
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
// orchestration error tests (no tmux session required)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires git + tmux"]
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
#[ignore = "requires git + tmux"]
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
#[ignore = "requires git + tmux"]
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

    // tmux session is live.
    assert!(
        env.window_exists(&window),
        "tmux session for '{window}' should exist"
    );

    // Give the shell a moment to echo, then peek should show the marker and the
    // file-indirection instruction the agent was launched with.
    std::thread::sleep(std::time::Duration::from_millis(800));
    let pane = env.capture(&window);
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
#[ignore = "requires git + tmux"]
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

    // `cat` reading stdin echoes whatever we send into the pane and stays
    // alive (tmux destroys a session whose process exits). Wrapping it in
    // `sh -c 'cat'` keeps cat argument-free — the read-this-file instruction
    // lands in `$0` — so it reads stdin instead of treating the instruction as
    // a filename and exiting.
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'cat'"])
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_millis(500));

    env.cmd()
        .args(["send", &name, "demo", "HELLO_FROM_SEND"])
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_millis(500));

    let pane = env.capture(&window);
    assert!(
        pane.contains("HELLO_FROM_SEND"),
        "sent text should appear in the pane; got:\n{pane}"
    );
}

#[test]
#[ignore = "requires git + tmux"]
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

    env.cmd()
        // A genuinely long-lived agent so the session stays up for the check;
        // the read-this-file instruction lands in `$0` and is ignored.
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.window_exists(&window),
        "window should exist after spawn"
    );

    env.cmd()
        .args(["kill", &name, "demo", "--rm-worktree"])
        .assert()
        .success();

    assert!(
        !env.window_exists(&window),
        "window should be gone after kill"
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
#[ignore = "requires git + tmux"]
fn cleanup_dry_run_reports_without_removing() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let window = format!("{name}-demo");

    // A branch spawned from `main` points at main's tip, so it is an ancestor
    // of origin/main — classified "merged". That alone is no longer enough to
    // reap: cleanup also requires the cook to be inactive. Report DONE so the
    // ticket becomes a genuine reap candidate for the dry run to spare.
    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.window_exists(&window),
        "window should exist after spawn"
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

    // The window, worktree, and registry entry all survive the dry run.
    assert!(
        env.window_exists(&window),
        "dry run must not kill the window"
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
#[ignore = "requires git + tmux"]
fn cleanup_yes_reaps_merged_ticket() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    let window = format!("{name}-demo");

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.window_exists(&window),
        "window should exist after spawn"
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

    // Session, worktree, and registry entry are all gone.
    assert!(
        !env.window_exists(&window),
        "window should be killed by cleanup"
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
#[ignore = "requires git + tmux"]
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

    let window = format!("{name}-demo");

    env.cmd()
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    assert!(
        env.window_exists(&window),
        "window should exist after spawn"
    );

    // Even with --yes, the merged-but-active ticket is kept, and the report
    // says why. (The summary line always contains the word "reaped" — e.g.
    // "0 reaped, 1 kept" — so we assert on the absence of the per-ticket reap
    // line for this branch specifically.)
    env.cmd()
        .args(["cleanup", &name, "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("keep   {name}/demo")))
        .stdout(predicate::str::contains("active"))
        .stdout(predicate::str::contains(format!("reaped {name}/demo")).not());

    // Window, worktree, and registry entry all survive.
    assert!(
        env.window_exists(&window),
        "active ticket's window must survive"
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

#[test]
#[ignore = "requires git + tmux"]
fn spawn_duplicate_window_rejected() {
    let env = TestEnv::new();
    env.init();
    let repo = SampleRepo::new();
    let name = unique_name();
    env.cmd()
        .args(["project", "add", &repo.url(), &name])
        .assert()
        .success();

    env.cmd()
        // A genuinely long-lived agent so the session stays up for the check;
        // the read-this-file instruction lands in `$0` and is ignored.
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .success();
    env.cmd()
        // A genuinely long-lived agent so the session stays up for the check;
        // the read-this-file instruction lands in `$0` and is ignored.
        .args(["spawn", &name, "demo", "--agent", "sh -c 'sleep 30'"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}
