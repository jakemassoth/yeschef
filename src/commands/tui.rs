//! Interactive TUI (`yeschef tui`): a sidebar of line cooks and a live view of
//! the selected line cook's session output.
//!
//! The pure UI state lives in [`App`] and is unit-tested without a terminal.
//! The crossterm raw-mode + event loop + ratatui rendering is a thin shell
//! around it.

use std::io;
use std::time::Duration;

use ansi_to_tui::IntoText;
use anyhow::{Context, Result};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::config::Config;
use crate::names::yeschef_session;

/// How often the pane refreshes when there's no input.
const TICK: Duration = Duration::from_secs(1);

/// How many trailing pane lines to capture for the live view.
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

/// One line cook row in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookRow {
    pub project: String,
    pub branch: String,
    pub window: String,
    pub state: CookState,
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
            }
        })
        .collect()
}

/// Capture the selected line cook's pane into the app (empty if none/error).
fn recapture(config: &Config, app: &mut App) {
    let pane = match app.selected_window() {
        Some(window) => config
            .zmx
            .capture_pane_styled(yeschef_session(), window, Some(CAPTURE_LINES))
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

fn run_loop<B: Backend>(config: &Config, terminal: &mut Terminal<B>) -> Result<()> {
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

fn ui(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(frame.area());

    render_sidebar(frame, app, chunks[0]);
    render_pane(frame, app, chunks[1]);
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

fn render_pane(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let title = app.selected_cook().map_or_else(
        || " no line cooks — spawn one with 'yeschef spawn' ".to_string(),
        |c| format!(" {}/{} [{}] ", c.project, c.branch, c.state.label()),
    );
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(area);

    // The pane is raw terminal scrollback: it carries ANSI/SGR escape codes for
    // colour and styling. Parse them into styled spans so the view renders in
    // colour instead of showing the codes as mangled text. Fall back to plain
    // text if a byte stream doesn't parse (e.g. truncated escape sequences).
    let text = app
        .pane()
        .into_text()
        .unwrap_or_else(|_| Text::raw(app.pane()));

    // Anchor to the bottom of the scrollback: scroll past everything that
    // doesn't fit. Long lines wrap (see `.wrap` below), so count *wrapped*
    // rows, not source lines, or the bottom won't actually be in view.
    let width = usize::from(inner.width).max(1);
    let visible = usize::from(inner.height);
    let rows: usize = text
        .lines
        .iter()
        .map(|line| {
            let w = line.width();
            if w == 0 {
                1
            } else {
                w.div_ceil(width)
            }
        })
        .sum();
    let scroll = rows.saturating_sub(visible);
    let scroll_y = u16::try_from(scroll).unwrap_or(u16::MAX);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0));
    frame.render_widget(paragraph, area);
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
    fn selected_window_tracks_selection() {
        let mut app = app_with(3);
        assert_eq!(app.selected_window(), Some("proj-b0"));
        app.select_next();
        assert_eq!(app.selected_window(), Some("proj-b1"));
    }
}
