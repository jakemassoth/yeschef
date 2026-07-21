# Investigation: replacing yeschef's tmux TUI/orchestration with `herdr`

**Status:** investigation + first concrete step (flake integration). No existing
behaviour changed; nothing in yeschef links or depends on herdr yet.

**Question asked:** Can [`herdr`](https://github.com/ogulcancelik/herdr) replace
yeschef's tmux-based TUI/session layer, and could that let us **drop SQLite**?

**Short answer:** Yes to the TUI/session layer — herdr is a purpose-built
superset of what yeschef's tmux backend does, and it does several things
(live agent-status detection, conversation-resuming restart, a git-branch-aware
sidebar) that yeschef currently hand-rolls or approximates. SQLite *can* be
dropped, but **not for free** and **not in one step**: herdr subsumes the ticket
*topology* (worktree ↔ workspace ↔ pane ↔ agent) and *status*, but it does **not**
own yeschef's *project registry* (`name → git_url → bare clone`) or the
merged/gone branch classification `cleanup` relies on. Those are yeschef's own
domain and need a home regardless. Recommendation: adopt herdr behind yeschef's
existing backend seam in phases, keep SQLite until the herdr backend is proven,
then re-evaluate the drop as a separate, decision-heavy change.

The single biggest strategic risk is **licensing** (herdr is AGPL-3.0-or-later +
commercial dual-licensed); see [Risks](#risks--open-questions). It needs a human
decision before we ever distribute the herdr binary.

---

## 1. What herdr is (verified against `herdr 0.7.4`)

herdr calls itself an "agent multiplexer that lives in your terminal": a single
Rust binary (no Electron) that manages a hierarchy of **workspaces → tabs →
panes**, detects the identity and lifecycle state of coding agents running in
those panes, and exposes the whole thing over a **socket API** with a scriptable
CLI. It is its own terminal multiplexer — it does **not** wrap tmux.

Facts below were checked by building herdr through this flake
(`nix build .#herdr` → `herdr-0.7.4`) and reading its `--help`,
`--default-config`, and its [SKILL.md](https://github.com/ogulcancelik/herdr/blob/master/SKILL.md).

### Architecture

- **Client/server.** A background **server** holds the running panes/agents in
  memory; the CLI and TUI are clients talking to it over a socket
  (`HERDR_SOCKET_PATH`). `herdr server` runs it headless; `herdr server stop`,
  `herdr server reload-config`, and `herdr status [server|client]` manage it.
- **Named persistent sessions.** `--session <name>` uses/creates a named
  persistent server; `herdr session {list,attach,stop,delete}` manage them.
  `--no-session` is a monolithic escape hatch (no server/client). This is the
  isolation primitive analogous to yeschef's private tmux `-L` socket.
- **Bare `herdr` launches/attaches the TUI** — never run it for scripting; use
  the subcommands.

### CLI surface (command groups)

| Group | Notable subcommands | Purpose |
|---|---|---|
| `pane` | `split`, `run`, `send-text`, `send-keys`, `read`, `wait-output`, `rename`, `close`, `list`, `get`, `report-agent`, `report-agent-session`, `report-metadata` | Terminal pane control + agent self-reporting |
| `agent` | `list`, `get`, `read`, `send-keys`, `prompt`, `wait`, `rename`, `focus`, `attach`, `start`, `explain` | Control/inspect agent panes |
| `workspace` | `create`, `list`, `get`, `focus`, `rename`, `report-metadata`, `close` | Workspace (space) management |
| `worktree` | `create`, `open`, `list`, `remove` | Git-worktree-backed workspaces |
| `tab` | `list`, … | Tabs within a workspace |
| `session` | `list`, `attach`, `stop`, `delete` | Named persistent servers |
| `notification` | … | Desktop/in-app alerts |
| `integration` | … | Built-in agent detection manifests |
| `api` | … | Inspect socket-API metadata + live runtime state |

Most helpers "operate over the socket API" and **return JSON**; per the SKILL,
you **parse IDs from JSON responses** rather than constructing them. Key flags
seen in the SKILL: `pane run <id> "<text>"` (sends text + Enter),
`pane read <id> --source [visible|recent|recent-unwrapped|detection] --lines <N>
--format [ansi|text]`, `agent wait <id> --status [idle|working|blocked|done|unknown]
--timeout <ms>`, `pane wait-output <id> --match "<pattern>" --timeout <ms>`.

### Agent detection + status

- herdr **detects agent identity and lifecycle state live**. Status values:
  `idle` (waiting, seen), `working` (active), `blocked` (needs input), `done`
  (completed, unseen), `unknown` (not detected).
- Built-in integrations recognise: `pi, claude, codex, gemini, cursor, devin,
  cline, opencode, copilot, kimi, kiro, droid, amp, grok, hermes, kilo,
  qodercli, qoder`. **Claude Code is first-class** — matches yeschef's default
  agent.
- Agents (or their supervisor) can also **explicitly report** state and identity:
  `herdr pane report-agent <state>`, `report-agent-session` (session ref for
  resume), `report-metadata` (arbitrary display key/values). herdr injects
  `$HERDR_WORKSPACE_ID`, `$HERDR_TAB_ID`, `$HERDR_PANE_ID` into every managed
  pane (active only when `HERDR_ENV=1`).
- The sidebar renders per-agent rows (`state_icon`, `state_text`, `workspace`,
  `tab`, `pane`, `agent`, …) and per-workspace rows including **`branch` and
  `git_status`** — i.e. herdr already shows the git branch and dirty-state of each
  worktree workspace, plus custom `$name` tokens fed via `report-metadata`.

### State persistence (the crux for the SQLite question)

- **Config:** `~/.config/herdr/config.toml` (TOML), override via
  `HERDR_CONFIG_PATH`. `[worktrees] directory = "~/.herdr/worktrees"`.
- **Session state:** JSON (`session.json`, and optionally `session-history.json`)
  in the config/session directory, **per named session**.
- **Detach/reattach:** normal detach keeps the server running — panes, shells,
  and agents keep running. Reconnect with `herdr`.
- **Server restart → snapshot restore:** restores the *shape* (workspaces, tabs,
  panes, cwds, layout, focus) but **not** the running processes; panes restart as
  fresh shells in their saved dirs.
- **Native agent session restore** (`[session] resume_agents_on_restore = true`,
  default on): for supported integrations, herdr persists a session ref and
  re-invokes the agent's native resume (e.g. `claude --resume <id>`) after a
  server restart. This is a **built-in, better version of yeschef's `restart
  --continue`**.
- **Pane history replay** (`[experimental] pane_history`, default **off** for
  security): replays recent terminal contents after a restart.
- **Live handoff** (experimental): `herdr update --handoff` moves running
  processes across a server replacement.

**Bottom line on persistence:** herdr durably persists the *topology + agent
identity/session refs + reported metadata*, per named session, as JSON. Agent
lifecycle *status* is detected live (or self-reported); it is not an authoritative
persisted field across a restart, but it is always queryable live via
`agent list`.

---

## 2. yeschef today (current architecture)

yeschef is a CLI orchestrating coding agents across git worktrees **via tmux**.
External I/O sits behind two traits in `src/backend/mod.rs`:

- **`GitBackend`** — bare clone, worktree add/remove, branch delete, config,
  default-branch detection, `fetch --prune`, and **branch classification**
  (`Merged`/`Gone`/`Unmerged`) that gates `cleanup`.
- **`TmuxBackend`** — one `yeschef` tmux session on a private `-L` socket with
  yeschef's own `tmux.conf`: head chef at window 0, one window per cook.
  `ensure_session`, `new_window`, `respawn_window` (restart-in-place),
  `send_keys`, `capture_pane`, `set_window_status` (colour-coded tab via a
  `@status` user option), `list_windows`, `kill_window`, `attach`.

**The TUI is the tmux status line** — no rendering code of yeschef's own. Tabs
are coloured from each window's `@status`, pushed by `ticket … status-set`.

**State (SQLite, `~/yeschef/yeschef.db`, `src/store.rs`), two tables:**

- `projects (name PRIMARY KEY, git_url)` — the project registry. Bare-clone and
  worktree *paths* are **derived** from `name` (`~/yeschef/projects/<name>/…`),
  not stored.
- `branches (project, branch, sanitized, window, agent, status, PK(project,branch))`
  — the ticket registry: maps a `(project, branch)` to its tmux window name, the
  agent command, and a **self-reported task status** (`NEW`/`IN_PROGRESS`/
  `DONE`/`BLOCKED`) that is *orthogonal to process liveness* and gates `cleanup`.

**Command → backend/store wiring** (`src/commands/orchestrate.rs`):

| Command | What it does | tmux | git | store |
|---|---|---|---|---|
| `spawn` | worktree + window + launch agent + register | ensure_session, window_exists, new_window | add_worktree, unset_config | project_exists, register_ticket |
| `send` | one line to a cook | send_keys | — | lookup_ticket |
| `peek` | recent pane output | capture_pane | — | lookup_ticket |
| `status` | table of tickets × liveness | list_windows, capture_pane | — | list_tickets |
| `ticket status-set` | self-reported status → recolour tab | set_window_status | — | set_ticket_status |
| `tui`/`attach` | hand terminal to tmux | ensure_session, attach | — | (lookup for `-t`) |
| `restart` | respawn every live agent, resume convo | list_windows, respawn_window | — | list_tickets |
| `kill` | stop window, optional rm worktree | kill_window | remove_worktree | remove_ticket |
| `project add`/`list`, `refresh` | registry + fetch | — | clone_bare, fetch_prune, … | add/list_project |
| `cleanup` | reap merged/gone **and** `DONE` tickets | kill_window | fetch_prune, branch_status | list/remove |

---

## 3. Capability mapping — herdr vs. yeschef's tmux layer

| yeschef need | tmux backend (today) | herdr equivalent | Fit |
|---|---|---|---|
| Brigade container | one `yeschef` session (private `-L` socket) | a named `--session` (persistent server) | ✅ clean |
| Head chef pinned | window 0 | a pane/workspace, or leave the head chef outside herdr | ✅ (design choice) |
| One cook = one window | `new_window` per ticket | `worktree create` (worktree **+** workspace) or `pane split` | ✅ **better** — herdr makes the worktree *and* the workspace in one call |
| Launch agent w/ brief | command string in the window | `pane run` / `agent start` / `agent prompt` | ✅ clean |
| Send a steer | `send_keys` | `pane run` / `agent send-keys` | ✅ clean |
| Peek output | `capture_pane -S -` trimmed | `pane read --source recent --lines N` | ✅ **richer** (visible/recent/detection sources) |
| Liveness / identity | `list_windows` + name match | `agent list` / `workspace list` (JSON) | ✅ **richer** (identity + live status) |
| Status decoration | `@status` glyph via `set_window_status` | native live detection + `report-agent`/`report-metadata`, rendered in sidebar | ✅ **better** — auto-detected; self-report still available |
| Restart, resume convo | `respawn_window` + `claude --continue` | server restart-restore + `resume_agents_on_restore` (native resume) | ✅ **better & built-in** |
| Attach / detach | `tmux attach` / `prefix+d` | bare `herdr` / `herdr --session` / `prefix+q` | ✅ clean |
| Gap-free tab order | `renumber-windows on` | herdr manages its own sidebar order | ✅ n/a |
| Shift+Enter to agent | `extended-keys` in `tmux.conf` | herdr's own input handling (verify per agent) | ⚠️ validate |
| Wait for a state | (none — yeschef polls) | `agent wait --status …`, `pane wait-output` | ✅ **new capability** |
| **Bare clone per project** | — | — (herdr has no project-registry concept) | ❌ stays in yeschef |
| **fetch --prune / default branch** | GitBackend | — | ❌ stays in yeschef |
| **merged/gone classification** | GitBackend (`branch_status`) | — | ❌ stays in yeschef |

**Reading of the table:** herdr cleanly subsumes the **entire `TmuxBackend`**, and
improves on three fronts yeschef built by hand — status (auto-detected vs.
self-reported + a `tmux.conf` colour hack), restart (native conversation resume
vs. our `--continue` special-case), and worktree creation (one call vs. git +
tmux in two). It also *adds* first-class "wait for status/output" primitives
yeschef lacks. What herdr does **not** do is yeschef's **git project model**:
bare-clone-per-project, fetch/refresh, default-branch detection, and the
merged/gone branch classification that makes `cleanup` safe. `GitBackend` stays.

---

## 4. Can we drop SQLite? — honest assessment

Split the two tables:

### `branches` (the ticket registry) — **subsumable**

Every column has a herdr home:

- `window` (tmux target) → replaced by herdr's own **pane/workspace/agent IDs**,
  which herdr persists in `session.json`. yeschef would resolve `(project,
  branch)` → herdr ID by querying `workspace list` / `agent list` and matching on
  the workspace name (encode `<project>/<branch>` as the workspace name) or on a
  `report-metadata` key. IDs are opaque and must be parsed from JSON — so this is
  a **label→ID lookup**, not a stored mapping.
- `agent` (the command) → herdr persists the agent identity + session ref for its
  own restore; yeschef needs this today mainly for `restart`, which herdr does
  natively. The need largely evaporates.
- `sanitized` → an artefact of deriving a tmux target; unnecessary once herdr owns
  identity.
- `status` → **either** adopt herdr's live-detected lifecycle state (`agent list`)
  **or** keep an explicit self-report via `herdr pane report-agent` /
  `report-metadata status=…`, which herdr persists as pane metadata and renders in
  its sidebar. No SQLite column needed. **But note the semantic gap** (below).

### `projects` (the registry) — **NOT subsumable**

herdr's `worktree create` operates on a repo, but herdr has **no first-class
"project = name → git_url → bare clone" concept**, no `fetch --prune`, and no
merged/gone classification. yeschef's whole project model (add = bare clone;
spawn = worktree off it; refresh = fetch; cleanup = classify) is git plumbing
herdr does not replicate. This registry is tiny (`name → git_url`) but real, and
it **needs a home** whether or not SQLite goes — the natural replacement is a
small `projects.toml`/`.json`.

### The semantic gap on status

yeschef's `DONE` means *"the work is finished and the PR is open"* — a claim the
line cook makes about its **task**. herdr's `done` means *"the agent process went
idle/finished, unseen"* — a fact about the **process**. They overlap but are not
the same; `cleanup`'s status-gated reaping depends on the *task* meaning. If we
lean on herdr's detection alone we lose that distinction; keeping an explicit
`report-agent`/`report-metadata` self-report preserves it. This is a **design
decision**, not a mechanical port.

### Verdict

Dropping SQLite is **plausible and attractive**, but it trades yeschef's
standalone durable ledger for *"query a running herdr server for topology/status
+ a tiny projects file."* That introduces a hard runtime dependency on a live
herdr server to resolve ticket identity (today `status`/`send`/`peek` work off
SQLite with tmux merely queried for liveness). That trade-off is defensible but
should be a **deliberate, late-phase decision** — not bundled with the backend
swap. Keep SQLite through the swap; re-evaluate once the herdr backend is proven.

---

## 5. Recommendation

**Adopt herdr as yeschef's session/TUI layer, in phases, behind the existing
backend seam — and reframe yeschef as a thin, opinionated orchestration layer on
top of herdr.**

herdr and yeschef overlap so heavily that the real question isn't "swap the TUI"
but "what is yeschef's residual value with herdr underneath?" That value is
genuine and worth keeping:

- The **head-chef / line-cook doctrine** and workflow (`AGENTS.md`): one ticket =
  one worktree, refined-prompt dispatch, the status protocol, the cleanup policy.
- The **git project model**: bare-clone-per-project, `refresh`, and the
  merged/gone classification `cleanup` needs (herdr has none of this).
- The **prompt-file mechanism** (write the brief to a file outside the worktree;
  launch the agent with a short "read this brief" instruction).
- yeschef's **CLI ergonomics** (`spawn`/`send`/`peek`/`status`/`kill` keyed by
  `project`+`branch`).

herdr takes over: session/window management, TUI rendering, live agent-status
detection, send/peek/read, restart-with-resume, attach/detach, and worktree
creation. Net effect once complete: delete `tmux.conf`, the tmux backend, the
`@status` colour machinery, and the bespoke `restart` logic.

This should happen **incrementally, feature-flagged, with tmux as the default
until herdr parity is proven**, and with three explicit human-approval gates:
licensing, the tmux deletion, and the SQLite drop.

---

## 6. Phased migration plan

### Phase 0 — Flake integration + this writeup ✅ (this PR)

herdr is buildable/runnable through yeschef's flake (`nix run .#herdr`); this
document lands. **No behaviour change**, nothing depends on herdr. See §7.

### Phase 1 — Manual validation, no yeschef code (needs licensing signoff first)

Stand up a **throwaway** herdr server (unique `--session` + `HERDR_CONFIG_PATH`,
or `--no-session`) and drive the exact loop yeschef needs, capturing the JSON
contract as fixtures:

1. `worktree create` → confirm it makes the git worktree **and** a workspace;
   capture IDs.
2. `agent start` / `pane run` → launch `claude` with a "read this brief"
   instruction; confirm detection identifies it as `claude`.
3. `agent read` / `pane read --source recent` → peek.
4. `pane run` / `agent send-keys` → steer.
5. `agent list` / `workspace list` → the `status` table; record JSON shape +
   which fields carry identity, branch, git status, and lifecycle state.
6. `agent wait --status …` → status transitions (new capability).
7. `pane report-agent` / `report-metadata` → self-report task status; confirm it
   renders and persists.
8. Server restart → confirm `resume_agents_on_restore` resumes the claude convo.
9. `session stop` / `delete` → teardown.

**Gate:** does the JSON contract cover everything yeschef needs, and does Claude
Code detection + native resume behave? Decide the identity-encoding scheme
(workspace name vs. reported metadata). Deliverable: a validation note + captured
JSON fixtures.

### Phase 2 — `HerdrBackend` behind a trait; SQLite kept; tmux still default

- Introduce a `SessionBackend` seam (generalise `TmuxBackend`, or add a sibling
  trait) with a `HerdrBackend` impl that shells out to `herdr` exactly as the real
  tmux backend shells out to `tmux` — **arm's-length CLI invocation, never linking
  herdr as a library** (licensing, see Risks).
- Route `spawn`/`send`/`peek`/`status`/`attach`/`tui` through herdr behind an
  env-var/config flag; **tmux stays the default.**
- Resolve `(project, branch)` → herdr ID via a `workspace list` lookup; to start,
  cache the resolved ID in the existing `window` column (no schema change).
- **Retire yeschef's `restart`** in favour of herdr's server restart-restore +
  `resume_agents_on_restore`.
- Re-establish the **test-isolation safety model** on herdr: the e2e suite must
  use a unique `--session` + `HERDR_CONFIG_PATH`/`XDG_CONFIG_HOME` per test (the
  analog of today's throwaway `-L` socket), and must never touch a live server.
  `herdr server stop` / `session delete` are the new "kill-server" foot-guns to
  guard.
- Keep `GitBackend` and SQLite unchanged.

**Gate:** herdr backend reaches parity with tmux on the full loop, with mock +
e2e coverage, before it can become the default.

### Phase 3 — Make herdr the default; delete the tmux layer (needs approval)

Once proven: flip the default to herdr, then remove `tmux.conf`, the tmux
`RealBackend`, `set_window_status`, and the `restart` command/logic; reduce
`tui`/`attach` to thin `herdr`/`herdr --session attach` shims. Large deletion,
decision-heavy — human/head-chef signoff.

### Phase 4 — Evaluate dropping SQLite (most decision-heavy; approval required)

Move the `projects` registry to a small `projects.toml`; let herdr own the ticket
topology + status; remove the `branches` table; keep `cleanup`'s branch
classification in `GitBackend`. Decide the canonical status model (herdr live
detection vs. self-reported task status). **Keep the door open to *keeping*
SQLite** as yeschef's durable ledger if "depend on a live herdr server for
identity" proves unacceptable.

---

## 7. What this PR actually changes (the concrete first step)

Only `flake.nix` (+ `flake.lock`) and this document. herdr is added as a flake
input and re-exposed so it is a runnable **through yeschef's flake**:

- **Input:** `herdr.url = "github:ogulcancelik/herdr"`. Deliberately **not**
  `follows`-ing yeschef's `nixpkgs`/`rust-overlay`: herdr pins its own toolchain
  (`rust-toolchain.toml` + zig/cmake native deps), so we let it build exactly as
  upstream locks it. (Deduping via `follows` is a future optimisation to
  validate, not a first-step requirement — it risks breaking herdr's pinned
  build.)
- **`packages.herdr`** — `herdr.packages.<system>.default`, so `nix build .#herdr`
  works. Verified: builds to `herdr-0.7.4`.
- **`apps.herdr`** — `herdr.apps.<system>.default`, so **`nix run .#herdr`** (and
  `nix run .#herdr -- <args>`) launches herdr. `nix run github:ogulcancelik/herdr`
  also works directly, since herdr's own flake exposes the same default app.
- **Intentionally kept out of `checks` and `devShells.default`** so the normal
  `nix flake check` / `nix develop` loop never has to compile herdr (a heavy
  zig/cmake build). You only pay for it when you explicitly ask for `.#herdr`.

Nothing in yeschef's Rust sources references herdr. This is pure availability.

---

## Risks & open questions

1. **Licensing — the big one, needs a human decision.** herdr is
   **AGPL-3.0-or-later + commercial** dual-licensed; yeschef currently ships **no
   license** (private / all-rights-reserved). Invoking `herdr` as an external CLI
   (like `git`/`tmux`) is **arm's-length aggregation** — it does *not* make
   yeschef a derivative work or impose AGPL on yeschef's own source. **But:**
   (a) *distributing* the herdr binary (e.g. baking it onto the default binary's
   PATH the way we do tmux, or shipping it) redistributes an AGPL work and must
   honour AGPL terms; (b) ever *linking/embedding* herdr code would pull AGPL into
   yeschef. **Mitigation:** keep it strictly arm's-length (shell out to the CLI),
   do **not** bake herdr onto the default binary's PATH, and get human/legal
   signoff before distributing. The commercial license is an escape hatch. This
   is why Phase 1 is gated on a licensing decision.
2. **Maturity / API stability.** herdr is `0.7.4` (pre-1.0); tmux is decades
   stable. herdr's socket-API JSON contract and CLI may change between releases,
   and yeschef would couple to it (parsing JSON IDs). **Mitigation:** pin via the
   flake (done), treat the JSON contract as a versioned dependency, add contract
   tests, capture fixtures in Phase 1.
3. **Runtime coupling to a daemon.** Today yeschef shells out to stateless `tmux`
   commands. herdr introduces a long-lived **server** to start, health-check, and
   reload. `status`/`send`/`peek` would depend on it being up. More moving parts.
4. **Test-isolation & the shared-kitchen safety invariant.** yeschef's entire
   test-safety story is the private per-test `-L` socket. The herdr analog
   (`--session <name>` + `HERDR_CONFIG_PATH`/`XDG_CONFIG_HOME`, or `--no-session`)
   maps, but the live-kitchen "never touch the shared server" invariant must be
   re-established for herdr before any e2e migration, with `herdr server stop` /
   `session delete` as the guarded foot-guns.
5. **Heavier dependency / closure.** herdr pulls a second nixpkgs + a zig/cmake
   Rust build into the flake closure; tmux is a tiny ubiquitous package. We kept
   herdr out of `checks`/`devShell` to avoid paying this on the normal loop, but a
   full migration makes herdr a hard runtime requirement.
6. **Loss of the standalone ledger if SQLite is dropped.** yeschef would depend on
   a running herdr server to resolve ticket identity; SQLite today is an
   independent source of truth. Weigh in Phase 4.
7. **Status semantics.** herdr's live process-lifecycle status vs. yeschef's
   self-reported *task* status (`DONE` = "PR open") are not the same; `cleanup`
   depends on the task meaning. Decide the canonical model before leaning on
   detection alone.
8. **Head-chef placement.** Does the head chef live as a herdr pane/workspace, or
   stay outside herdr entirely? A design choice for Phase 2.
9. **Shift+Enter / key forwarding.** yeschef relies on `tmux.conf`'s
   `extended-keys` so Claude Code sees Shift+Enter. Verify herdr forwards the
   equivalent CSI-u sequences per agent (Phase 1).

## Appendix — how to reproduce the herdr facts here

```sh
# Build + run herdr through this flake (heavy first build; ~herdr-0.7.4):
nix build .#herdr
nix run   .#herdr -- --help
nix run   .#herdr -- --default-config
# Isolate any manual poking from your real herdr config/server:
export XDG_CONFIG_HOME="$(mktemp -d)"   # or HERDR_CONFIG_PATH=...
nix run .#herdr -- --session throwaway-$$ ...   # never a shared/live session
```
