//! Interactive TUI (`yeschef tui`): a sidebar of line cooks and a live view of
//! the selected line cook's session output.
//!
//! The pure UI state lives in [`App`] and is unit-tested without a terminal.
//! The crossterm raw-mode + event loop + ratatui rendering is a thin shell
//! around it.
//!
//! ## Interaction model (mprocs-style)
//!
//! List mode navigates the brigade (`j`/`k`/`g`/`G`) while the right-hand pane
//! passively previews the selected line cook's session, colours and all, via
//! a real VT100 parser ([`vt100`] + [`tui_term`]) rather than treating the
//! captured scrollback as plain wrappable text. Pressing `Enter` hands the
//! *real* terminal to `zmx attach` for full-fidelity, full-keyboard
//! interaction with that session — see [`focus_session`] for why that's the
//! right call given what zmx exposes. Leaving focus is zmx's own detach
//! (`Ctrl+\`), which returns here automatically.

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use tui_term::widget::PseudoTerminal;

use crate::config::Config;
use crate::names::yeschef_session;

/// How often the pane refreshes when there's no input.
const TICK: Duration = Duration::from_secs(1);

/// How many trailing rows of scrollback the pane's VT100 parser retains.
/// The parser is rebuilt from a full replay every refresh (see
/// [`render_pane`]), so this bounds memory/CPU rather than acting as a
/// display window — the visible rows are whatever the parser's live screen
/// currently shows, always anchored to the bottom.
const CAPTURE_LINES: usize = 500;

/// Liveness of a line cook's window, mirroring `run_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookState {
    Running,
    Dead,
    Gone,
}

impl CookState {
    fn label(self) -> &'static str {
        match self {
            CookState::Running => "running",
            CookState::Dead => "dead",
            CookState::Gone => "gone",
        }
    }

    fn color(self) -> Color {
        match self {
            CookState::Running => Color::Green,
            CookState::Dead => Color::Red,
            CookState::Gone => Color::DarkGray,
        }
    }
}

/// A line cook's self-reported task status, orthogonal to [`CookState`]
/// (which is window liveness). Parsed from the stored `status` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    New,
    InProgress,
    Done,
    Blocked,
}

impl TaskStatus {
    /// Parse the stored status string; anything unrecognized (including the
    /// empty/`NEW` default) reads as [`TaskStatus::New`].
    fn from_stored(s: &str) -> Self {
        match s {
            "IN_PROGRESS" => TaskStatus::InProgress,
            "DONE" => TaskStatus::Done,
            "BLOCKED" => TaskStatus::Blocked,
            _ => TaskStatus::New,
        }
    }

    fn label(self) -> &'static str {
        match self {
            TaskStatus::New => "NEW",
            TaskStatus::InProgress => "IN_PROGRESS",
            TaskStatus::Done => "DONE",
            TaskStatus::Blocked => "BLOCKED",
        }
    }

    fn color(self) -> Color {
        match self {
            TaskStatus::New => Color::DarkGray,
            TaskStatus::InProgress => Color::Yellow,
            TaskStatus::Done => Color::Green,
            TaskStatus::Blocked => Color::Red,
        }
    }
}

/// One line cook row in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookRow {
    pub project: String,
    pub branch: String,
    pub window: String,
    pub state: CookState,
    pub status: TaskStatus,
}

/// Pure UI state: the brigade list, the selected index, and the captured pane
/// text for the selected line cook. Constructible and testable without a
/// terminal.
#[derive(Debug, Default)]
pub struct App {
    brigade: Vec<CookRow>,
    selected: usize,
    pane: String,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn brigade(&self) -> &[CookRow] {
        &self.brigade
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn pane(&self) -> &str {
        &self.pane
    }

    /// The currently-selected line cook, if the brigade list is non-empty.
    pub fn selected_cook(&self) -> Option<&CookRow> {
        self.brigade.get(self.selected)
    }

    /// The window name to capture for the current selection.
    pub fn selected_window(&self) -> Option<&str> {
        self.selected_cook().map(|c| c.window.as_str())
    }

    /// Replace the brigade list, keeping the selection in bounds.
    pub fn set_brigade(&mut self, brigade: Vec<CookRow>) {
        self.brigade = brigade;
        self.clamp_selection();
    }

    pub fn set_pane(&mut self, pane: String) {
        self.pane = pane;
    }

    /// Keep `selected` pointing at a valid row (or 0 when the list is empty).
    fn clamp_selection(&mut self) {
        if self.brigade.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.brigade.len() {
            self.selected = self.brigade.len() - 1;
        }
    }

    pub fn select_next(&mut self) {
        if self.brigade.is_empty() {
            return;
        }
        if self.selected + 1 < self.brigade.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.brigade.len().saturating_sub(1);
    }
}

/// Build the brigade list from the store + the live window states.
fn load_brigade(config: &Config) -> Vec<CookRow> {
    let tickets = config.store.list_tickets().unwrap_or_default();
    let windows = config
        .zmx
        .list_windows(yeschef_session())
        .unwrap_or_default();
    tickets
        .into_iter()
        .map(|ticket| {
            let state = match windows.iter().find(|w| w.name == ticket.window) {
                Some(w) if w.dead => CookState::Dead,
                Some(_) => CookState::Running,
                None => CookState::Gone,
            };
            CookRow {
                project: ticket.project,
                branch: ticket.branch,
                window: ticket.window,
                state,
                status: TaskStatus::from_stored(&ticket.status),
            }
        })
        .collect()
}

/// Capture the selected line cook's pane into the app (empty if none/error).
/// Styled (VT/ANSI), not plain — see [`render_pane`] for why.
fn recapture(config: &Config, app: &mut App) {
    let pane = match app.selected_window() {
        Some(window) => config
            .zmx
            .capture_pane_styled(yeschef_session(), window)
            .unwrap_or_default(),
        None => String::new(),
    };
    app.set_pane(pane);
}

/// Reload the brigade list and re-capture the selected pane.
fn refresh(config: &Config, app: &mut App) {
    app.set_brigade(load_brigade(config));
    recapture(config, app);
}

/// Entry point for `yeschef tui`. Sets up the terminal, runs the event loop,
/// and always restores the terminal afterwards.
pub fn run_tui(config: &Config) -> Result<()> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    let result = run_loop(config, &mut terminal);

    // Always restore the terminal, even if the loop errored.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

fn run_loop<B: Backend + io::Write>(config: &Config, terminal: &mut Terminal<B>) -> Result<()> {
    let mut app = App::new();
    refresh(config, &mut app);

    loop {
        terminal.draw(|frame| ui(frame, &app))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if ctrl => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down => app.select_next(),
                    KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
                    KeyCode::Char('g') => app.select_first(),
                    KeyCode::Char('G') => app.select_last(),
                    KeyCode::Enter => {
                        // Gone means the zmx session no longer exists — nothing
                        // to attach to. Dead (foreground process exited but the
                        // session lingers) still has a live shell worth visiting.
                        let window = app
                            .selected_cook()
                            .filter(|c| c.state != CookState::Gone)
                            .map(|c| c.window.clone());
                        if let Some(window) = window {
                            let _ = focus_session(config, terminal, &window);
                        }
                        refresh(config, &mut app);
                        continue;
                    }
                    _ => continue,
                }
                // Selection may have changed — update the pane immediately.
                recapture(config, &mut app);
            }
        } else {
            // Tick: state may have changed, so reload everything.
            refresh(config, &mut app);
        }
    }
}

/// Suspend the TUI and hand the real terminal to `zmx attach` for direct,
/// full-fidelity interaction with a line cook's session.
///
/// Why shell out instead of forwarding keys ourselves: zmx already owns the
/// session's pty and knows how to resize it and encode arbitrary keys
/// (arrows, function keys, ctrl combos) correctly — reimplementing that here
/// would mean rebuilding a terminal emulator. Attaching for real also fixes
/// the pty's column width to match this terminal, which is the one thing
/// that actually resolves the wrapping mismatch described in
/// [`render_pane`] (as opposed to working around it after the fact).
///
/// Leaving focus is zmx's own detach binding (`Ctrl+\`), deliberately not a
/// binding of ours: agents commonly use Ctrl+A/Ctrl+E/Esc for their own
/// readline/TUI editing, so intercepting one of those ourselves would steal
/// it from the agent. `Ctrl+\` is the same low-conflict escape hatch zmx
/// already chose for its own detach, so this is consistent with the tool
/// it's built on rather than inventing a second convention.
fn focus_session<B: Backend + io::Write>(
    config: &Config,
    terminal: &mut Terminal<B>,
    window: &str,
) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;

    let result = config.zmx.attach(yeschef_session(), Some(window));

    // Best-effort restore, mirroring `run_tui`'s teardown: if re-entering the
    // alternate screen also failed we're in worse trouble than a stale
    // attach error, but we still want to surface the attach outcome, not a
    // restore outcome, to the caller.
    let _ = enable_raw_mode();
    let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
    let _ = terminal.clear();

    result
}

fn ui(frame: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(rows[0]);

    render_sidebar(frame, app, chunks[0]);
    render_pane(frame, app, chunks[1]);
    render_footer(frame, rows[1]);
}

fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect) {
    let help = Line::styled(
        " j/k move  ·  g/G top/bottom  ·  Enter focus session  ·  Ctrl+\\ back to list  ·  q quit ",
        Style::default().fg(Color::DarkGray),
    );
    frame.render_widget(Paragraph::new(help), area);
}

fn render_sidebar(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = app
        .brigade()
        .iter()
        .map(|c| {
            let line = Line::from(vec![
                Span::styled(
                    c.project.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("/"),
                Span::raw(c.branch.clone()),
                Span::raw("  "),
                Span::styled(c.state.label(), Style::default().fg(c.state.color())),
                Span::raw(" "),
                Span::styled(c.status.label(), Style::default().fg(c.status.color())),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    if !app.brigade().is_empty() {
        state.select(Some(app.selected()));
    }

    let list = List::new(items)
        .block(Block::default().title(" brigade ").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut state);
}

/// Render the selected line cook's pane by replaying its captured VT/ANSI
/// scrollback through a real terminal-emulation parser ([`vt100`]) instead
/// of treating it as wrappable plain text.
///
/// This matters because `zmx history --vt` isn't "coloured text" — it's a
/// stateful replay stream (SGR colour codes interleaved with cursor-move and
/// erase operations, however the agent's own screen was drawn). A previous
/// attempt at this fix (branch `fix-tui-colors-wrap`) parsed that stream with
/// a generic ANSI-to-text crate and re-wrapped it with ratatui's `Paragraph`
/// `Wrap`, which looked fine for a simple synthetic demo but breaks on real,
/// richly-interactive agent output: cursor-addressed redraws aren't "long
/// lines" that can be safely re-flowed at a different width, and doing so
/// mangles box-drawing UI and misaligns content. Running the same bytes
/// through an actual VT100 state machine (as zmx/ghostty and mprocs both do
/// internally) resolves those operations into a concrete cell grid *before*
/// we ever try to display it, so what we render is correct content — no
/// re-wrap needed, since `vt100::Screen` already anchors to the live/bottom
/// view.
///
/// The real, structural limitation this can't paper over: the agent process
/// laid out that content assuming *its* pty's width, which zmx fixes once at
/// spawn time (from whatever spawned `zmx run`'s own tty happened to be, or
/// a 160x24 fallback if that wasn't a tty — see `zmx`'s `ipc.getTerminalSize`)
/// and — unlike mprocs, which owns its ptys and keeps them continuously
/// resized to the render area — exposes no way to resize a detached
/// session's pty afterwards short of a live attach. If that width doesn't
/// match this pane, content wider than our pane clips at the edge (graceful)
/// rather than getting corrupted (what the naive re-wrap did) — but it won't
/// be pixel-perfect. Focus mode (`Enter`, see [`focus_session`]) sidesteps
/// this entirely by attaching for real, which resizes the pty to match.
fn render_pane(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let title = match app.selected_cook() {
        None => Line::from(" no line cooks — spawn one with 'yeschef spawn' "),
        Some(c) => Line::from(vec![
            Span::raw(format!(" {}/{} [", c.project, c.branch)),
            Span::styled(c.state.label(), Style::default().fg(c.state.color())),
            Span::raw("] "),
            Span::styled(c.status.label(), Style::default().fg(c.status.color())),
            Span::raw(" "),
        ]),
    };
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(area);

    let cols = inner.width.max(1);
    let rows = inner.height.max(1);
    let mut parser = vt100::Parser::new(rows, cols, CAPTURE_LINES);
    parser.process(app.pane().as_bytes());

    let pseudo_term = PseudoTerminal::new(parser.screen()).block(block);
    frame.render_widget(pseudo_term, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(project: &str, branch: &str) -> CookRow {
        CookRow {
            project: project.to_string(),
            branch: branch.to_string(),
            window: format!("{project}-{branch}"),
            state: CookState::Running,
            status: TaskStatus::New,
        }
    }

    fn app_with(n: usize) -> App {
        let mut app = App::new();
        app.set_brigade((0..n).map(|i| row("proj", &format!("b{i}"))).collect());
        app
    }

    #[test]
    fn empty_list_navigation_is_noop() {
        let mut app = App::new();
        assert_eq!(app.selected(), 0);
        assert!(app.selected_cook().is_none());
        assert!(app.selected_window().is_none());

        app.select_next();
        app.select_prev();
        app.select_last();
        app.select_first();
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn select_next_stops_at_last() {
        let mut app = app_with(3);
        app.select_next();
        assert_eq!(app.selected(), 1);
        app.select_next();
        assert_eq!(app.selected(), 2);
        app.select_next(); // clamped
        assert_eq!(app.selected(), 2);
    }

    #[test]
    fn select_prev_stops_at_first() {
        let mut app = app_with(3);
        app.select_last();
        assert_eq!(app.selected(), 2);
        app.select_prev();
        assert_eq!(app.selected(), 1);
        app.select_prev();
        assert_eq!(app.selected(), 0);
        app.select_prev(); // saturates
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn first_and_last_jump() {
        let mut app = app_with(5);
        app.select_last();
        assert_eq!(app.selected(), 4);
        app.select_first();
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn set_brigade_clamps_selection_when_list_shrinks() {
        let mut app = app_with(5);
        app.select_last();
        assert_eq!(app.selected(), 4);

        // List shrinks below the selected index.
        app.set_brigade(vec![row("proj", "a"), row("proj", "b")]);
        assert_eq!(app.selected(), 1);

        // List empties out.
        app.set_brigade(vec![]);
        assert_eq!(app.selected(), 0);
    }

    #[test]
    fn set_brigade_keeps_valid_selection() {
        let mut app = app_with(5);
        app.select_next();
        app.select_next();
        assert_eq!(app.selected(), 2);
        // Same-size update keeps the selection put.
        app.set_brigade((0..5).map(|i| row("proj", &format!("x{i}"))).collect());
        assert_eq!(app.selected(), 2);
    }

    #[test]
    fn task_status_parses_and_colors() {
        assert_eq!(
            TaskStatus::from_stored("IN_PROGRESS"),
            TaskStatus::InProgress
        );
        assert_eq!(TaskStatus::from_stored("DONE"), TaskStatus::Done);
        assert_eq!(TaskStatus::from_stored("BLOCKED"), TaskStatus::Blocked);
        // The default and anything unknown read as New.
        assert_eq!(TaskStatus::from_stored("NEW"), TaskStatus::New);
        assert_eq!(TaskStatus::from_stored(""), TaskStatus::New);
        assert_eq!(TaskStatus::from_stored("garbage"), TaskStatus::New);

        assert_eq!(TaskStatus::InProgress.color(), Color::Yellow);
        assert_eq!(TaskStatus::Done.color(), Color::Green);
        assert_eq!(TaskStatus::Blocked.color(), Color::Red);
        assert_eq!(TaskStatus::New.color(), Color::DarkGray);
    }

    #[test]
    fn brigade_row_carries_task_status() {
        let mut app = App::new();
        let mut r = row("proj", "feature");
        r.status = TaskStatus::Blocked;
        app.set_brigade(vec![r]);
        let cook = app.selected_cook().unwrap();
        assert_eq!(cook.status, TaskStatus::Blocked);
        assert_eq!(cook.status.label(), "BLOCKED");
    }

    #[test]
    fn selected_window_tracks_selection() {
        let mut app = app_with(3);
        assert_eq!(app.selected_window(), Some("proj-b0"));
        app.select_next();
        assert_eq!(app.selected_window(), Some("proj-b1"));
    }

    /// `recapture` must fetch the VT-styled pane, not the plain one — the
    /// plain capture strips colour, which is exactly what the pane renderer
    /// needs preserved (see `render_pane`'s doc comment for why).
    #[test]
    fn recapture_uses_styled_capture_not_plain() {
        use crate::backend::mock::{MockGitBackend, MockZmxBackend};
        use crate::store::Store;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .add_project("proj", "https://example.com/proj.git")
            .unwrap();
        store
            .register_ticket("proj", "feature", "feature", "proj-feature", "claude")
            .unwrap();
        let zmx = MockZmxBackend::new().with_styled_pane(
            "yeschef",
            "proj-feature",
            "\x1b[32mhello\x1b[0m\n",
        );
        let config = Config {
            home: tmp.path().to_path_buf(),
            store,
            git: Box::new(MockGitBackend::new()),
            zmx: Box::new(zmx.clone()),
        };

        let mut app = App::new();
        refresh(&config, &mut app);

        assert_eq!(app.brigade().len(), 1);
        assert!(app.pane().contains("hello"));

        let calls = zmx.recorded_calls();
        assert!(
            calls
                .iter()
                .any(|c| c == "capture_pane_styled:yeschef:proj-feature"),
            "calls: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("capture_pane:")),
            "plain capture_pane should not be used for the TUI's live pane: {calls:?}"
        );
    }

    /// The pane renderer replays the styled capture through a real VT100
    /// parser rather than displaying it as raw text — colour codes should
    /// resolve to styled cells, not show up as literal escape bytes, and the
    /// plain text content should still be present.
    #[test]
    fn styled_pane_resolves_ansi_via_vt100() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"\x1b[32mhello\x1b[0m world\n");
        let screen = parser.screen();
        let cell = screen.cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "h");
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(2)); // green
        assert_eq!(screen.contents(), "hello world");
    }
}
