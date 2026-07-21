# Verification: the herdr flow works end to end

**Ticket:** prove the whole new herdr flow (herdr as yeschef's session/TUI backend,
PR #29, replacing tmux) works end to end. This is a QA/verification report, not a
feature.

**Verdict: VERIFIED.** Every stage of the lifecycle yeschef exposes on top of herdr
— `init`, `project add`/`list`, `refresh`, `spawn`, `peek`, `send`, `status`,
`ticket status-set`, `restart`, `kill`, `cleanup` — works end to end against a real
herdr `0.7.4` server. Confirmed at three levels: 55 mock-backed unit tests, 19
real-herdr e2e tests, and a hands-on isolated walkthrough that captured real command
output and pane text. No breakage was found. Two cosmetic, non-blocking observations
are noted below.

Verified on `origin/main` @ `582fd70` (branch `herdr-proof`, no source changes — this
report is the only addition), macOS (darwin 25.5.0), `herdr 0.7.4`.

---

## Isolation — the live kitchen was never touched

A real head-chef brigade was running throughout (`herdr session list` showed
`default` **running** and `yeschef` **running**). None of the verification touched
either. Every herdr operation was confined to a sandbox, exactly as the e2e suite
isolates itself:

- **Unique session name** per run (`YESCHEF_HERDR_SESSION=ysproof-<pid>` for the
  walkthrough; `yt-<pid>-<nanos>-<seq>` per-test in e2e) — never the live `yeschef`.
- **Private `XDG_CONFIG_HOME`** under `/tmp` (herdr derives its socket, session
  state, and logs from there; `/tmp` keeps the path under the unix-socket
  `sun_path` ~104-byte limit).
- **Private `YESCHEF_HOME`** (its own `yeschef.db`, `projects/`, `prompts/`).
- **Stand-in agents only** — the head chef ran `sh -c 'exec sleep 300'` and the
  line cooks ran `sh`/`cat`/`echo`/`sleep` loops. No real coding agent, no network,
  no external service.
- **Guaranteed teardown** — the walkthrough stops its own server and removes its
  sandbox on any exit; each e2e `TestEnv::drop` stops its server.

**Post-run check:** the live `default` and `yeschef` sessions were still `running`,
no `ysproof` session leaked into the live config, and no `/tmp/ysp.*` sandbox
remained.

---

## Level 1 — unit tests (mock-backed orchestration logic)

`cargo test --bin yeschef` → **55 passed, 0 failed.** These drive
`spawn`/`send`/`peek`/`status`/`status-set`/`kill`/`restart`/`tui` and the
`store`/`names`/`guard` logic against `MockHerdrBackend`/`MockGitBackend`, so the
orchestration wiring is exercised with no herdr server. Highlights: spawn creates the
workspace + registers the ticket with herdr's ids; the prompt is written to a file
and the agent launched via a short "read this brief" instruction (never inlined —
the ENAMETOOLONG guard); the head chef is ensured exactly once; `status-set` pushes
display metadata to the pane; `restart` bounces the server (stop → ensure).

## Level 2 — e2e tests (real herdr server, isolated per test)

`nix run .#e2e` (i.e. `cargo test --test e2e -- --ignored`) → **19 passed, 0
failed** in 5.5s. Each test drives a real detached herdr server on its own
throwaway session + config home. Coverage spans the whole flow:

| Stage | e2e test(s) |
|---|---|
| init layout / idempotency | `init_creates_expected_layout`, `init_is_idempotent` |
| project add / list / refspec | `project_add_registers_bare_clone`, `project_add_makes_origin_main_resolve`, `project_list_empty` |
| refresh repairs old clones | `refresh_repairs_clone_with_no_tracking_refspec` |
| spawn → worktree + live workspace + prompt file | `spawn_creates_worktree_and_live_workspace` |
| send reaches the pane | `send_reaches_the_pane` |
| kill closes workspace + deregisters | `kill_removes_workspace_and_deregisters` |
| restart restores the brigade | `restart_restores_the_brigade`, `restart_without_server_errors` |
| cleanup (dry-run / reap / keep-active) | `cleanup_dry_run_reports_without_removing`, `cleanup_yes_reaps_merged_ticket`, `cleanup_yes_keeps_active_ticket_even_when_merged` |
| error paths | `spawn_unknown_project_gives_clear_error`, `send_unknown_ticket_gives_clear_error`, `spawn_duplicate_workspace_rejected`, name validation, duplicate project |

## Level 3 — hands-on isolated walkthrough (captured evidence)

Drove the real CLI as a head chef would, against a throwaway local git repo, in the
sandbox described above. Every command succeeded (`exit=0`). Captured highlights:

- **`project add`** bare-cloned the repo and `origin/main` resolved in the bare
  clone (so `spawn --base origin/main` works).
- **`spawn demo feat-x -p "…"`** created the worktree, created the herdr workspace,
  and launched the stand-in agent. `herdr workspace list` (raw JSON) showed two
  workspaces — `headchef` (`w1`) and `demo/feat-x` (`w2`) — proving the pinned head
  chef is ensured alongside the cook. The prompt was written to
  `…/prompts/demo-feat-x.md` (outside the worktree) with the status-reporting
  preamble first and the verbatim user prompt after a `---` rule.
- **`peek`** showed the agent's real pane output: `SPAWN_OK_MARKER` and
  `launched-with: Read the ticket brief at …/prompts/demo-feat-x.md and carry it out
  start to finish.` — confirming the file-indirection launch reaches the agent as
  `$0`.
- **`send "HELLO_FROM_HEADCHEF"`** was typed into the pane and echoed back on the
  next `peek` — the steer path works.
- **`status`** listed the ticket as live (not `gone`); **`ticket … status-set
  IN_PROGRESS`** flipped the STATUS column and best-effort pushed display metadata to
  the pane.
- A **second cook** (`feat-y`) appeared as a second ticket — the brigade holds
  multiple cooks in one session.
- **`restart`** bounced the server; all three workspaces (`headchef`, `demo/feat-x`,
  `demo/feat-y`) were restored from herdr's persisted session and the SQLite-backed
  task STATUS survived the bounce.
- **`kill demo feat-x`** (no `--rm-worktree`) closed the workspace, kept the worktree
  on disk, and deregistered the ticket.
- **`cleanup`** dry-run reported `would reap demo/feat-y — merged and status DONE`
  and removed nothing; `cleanup --yes` then reaped it (`1 reaped, 0 kept`) and
  `status` went empty. (e2e separately proves an *active* merged ticket is kept.)

---

## Observations (non-blocking; not breakage)

1. **`agent_status` reads `unknown` for the stand-in agents.** herdr's live
   lifecycle detection only recognizes real agent integrations (claude, codex, …); a
   bare `sh`/`cat`/`sleep` isn't one, so the STATE column shows `unknown`. This is
   correct behavior, but it means this QA pass did **not** exercise herdr's
   `working`/`idle`/`blocked`/`done` detection or its native conversation-resume on
   restart — both require a real recognized agent (deliberately out of scope here, to
   avoid depending on external services). Everything yeschef itself drives (workspace
   liveness = present-vs-`gone`, task STATUS, restore-on-restart of the workspace
   shape) is fully verified; herdr's agent *detection*/resume is upstream behavior we
   rely on but did not re-prove with a live agent.

2. **`status` column alignment breaks with a long `--agent` string.** The AGENT
   column is fixed-width (`{:<10}`), so a long custom agent command (e.g.
   `sh -c 'echo …; cat'`) overflows and pushes the rest of the row out of alignment.
   Purely cosmetic — the default `claude` fits fine, and every field is still
   present and correct. Left as-is (out of scope for this verification ticket); a
   future polish could truncate/elide the agent string in the table.

No test was added: the existing unit + e2e suites already cover the full lifecycle
comprehensively, so a new test would be redundant.

---

## Reproduce

```sh
# Levels 1 + 2 (from the repo root):
nix develop -c cargo test --bin yeschef        # 55 unit tests
nix run .#e2e                                   # 19 e2e tests (real herdr, isolated)

# Level 3: an isolated manual walkthrough — set a UNIQUE session + private
# XDG_CONFIG_HOME (short, under /tmp) + private YESCHEF_HOME + stand-in agents,
# then drive init → project add → spawn → peek → send → status → status-set →
# restart → kill → cleanup, and stop your own server on exit. NEVER use the
# session name `yeschef` or the default XDG_CONFIG_HOME (that is the live brigade).
```
