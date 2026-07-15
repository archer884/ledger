use std::fmt::Write as _;
use std::io::{self, Stdout};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};
use rust_decimal::Decimal;
use thiserror::Error;

use crate::model::{Account, AccountId, Transaction};
use crate::storage::{Storage, StorageError};

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
}

enum View {
    Accounts,
    Register { account: AccountId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Search,
    DateFrom,
    DateTo,
}

struct App {
    storage: Storage,
    accounts_with_balances: Vec<(Account, Decimal)>,
    transactions: Vec<Transaction>,
    view: View,
    table_state: TableState,
    search: String,
    date_from: Option<Timestamp>,
    date_to: Option<Timestamp>,
    input_mode: InputMode,
    input_buffer: String,
    status: String,
}

impl App {
    fn new(storage: Storage) -> Result<Self, TuiError> {
        let mut app = Self {
            storage,
            accounts_with_balances: Vec::new(),
            transactions: Vec::new(),
            view: View::Accounts,
            table_state: TableState::default(),
            search: String::new(),
            date_from: None,
            date_to: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            status: String::new(),
        };
        app.load()?;
        app.reset_cursor();
        Ok(app)
    }

    fn load(&mut self) -> Result<(), TuiError> {
        let accounts = self.storage.list_accounts()?;
        let balances = self.storage.balances()?;
        let transactions = self.storage.list_transactions()?;

        let mut with_balances: Vec<(Account, Decimal)> = accounts
            .into_iter()
            .map(|a| {
                let balance = balances.get(&a.id).copied().unwrap_or(Decimal::ZERO);
                (a, balance)
            })
            .collect();
        with_balances.sort_by(|a, b| a.0.id.as_str().cmp(b.0.id.as_str()));

        self.accounts_with_balances = with_balances;
        self.transactions = transactions;
        Ok(())
    }

    fn reload(&mut self) {
        match self.load() {
            Ok(()) => {
                self.status = "reloaded".to_string();
                self.reset_cursor();
            }
            Err(e) => {
                self.status = format!("reload failed: {e}");
            }
        }
    }

    fn filtered_accounts(&self) -> Vec<(Account, Decimal)> {
        self.accounts_with_balances
            .iter()
            .filter(|(a, _)| self.matches_search(&a.id.to_string()))
            .cloned()
            .collect()
    }

    fn filtered_transactions(&self) -> Vec<Transaction> {
        let account = match &self.view {
            View::Register { account } => account.clone(),
            View::Accounts => return Vec::new(),
        };
        self.transactions
            .iter()
            .filter(|tx| tx.entries.iter().any(|e| e.account == account))
            .filter(|tx| {
                self.date_from.is_none_or(|from| tx.posted_at >= from)
                    && self.date_to.is_none_or(|to| tx.posted_at <= to)
            })
            .filter(|tx| self.matches_search(&tx.memo))
            .cloned()
            .collect()
    }

    fn matches_search(&self, s: &str) -> bool {
        if self.search.is_empty() {
            true
        } else {
            s.to_lowercase().contains(&self.search.to_lowercase())
        }
    }

    fn current_list_len(&self) -> usize {
        match self.view {
            View::Accounts => self.filtered_accounts().len(),
            View::Register { .. } => self.filtered_transactions().len(),
        }
    }

    fn reset_cursor(&mut self) {
        let len = self.current_list_len();
        if len > 0 {
            self.table_state.select(Some(0));
        } else {
            self.table_state.select(None);
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    fn move_cursor(&mut self, delta: i32) {
        let len = self.current_list_len();
        if len == 0 {
            self.table_state.select(None);
            return;
        }
        let current = self.table_state.selected().unwrap_or(0) as i32;
        let new = (current + delta).rem_euclid(len as i32) as usize;
        self.table_state.select(Some(new));
    }

    fn drill_in(&mut self) {
        if matches!(self.view, View::Accounts)
            && let Some(idx) = self.table_state.selected()
            && let Some((account, _)) = self.filtered_accounts().get(idx)
        {
            self.view = View::Register {
                account: account.id.clone(),
            };
            self.reset_cursor();
        }
    }

    fn drill_out(&mut self) {
        if matches!(self.view, View::Register { .. }) {
            self.view = View::Accounts;
            self.reset_cursor();
        }
    }

    fn status_line(&self) -> String {
        match self.input_mode {
            InputMode::Normal => {
                let mut s = String::new();
                if !self.search.is_empty() {
                    let _ = write!(s, "/{} ", self.search);
                }
                if let Some(from) = self.date_from {
                    let _ = write!(s, "from:{} ", format_date(from));
                }
                if let Some(to) = self.date_to {
                    let _ = write!(s, "to:{} ", format_date(to));
                }
                if !self.status.is_empty() {
                    let _ = write!(s, "[{}] ", self.status);
                }
                s.push_str(
                    "j/k: move  Enter: drill  Esc: back  /: search  f/t: date  c: clear  r: reload  q: quit",
                );
                s
            }
            InputMode::Search => format!("/{}  (Enter: confirm, Esc: cancel)", self.input_buffer),
            InputMode::DateFrom => format!(
                "from (YYYY-MM-DD, empty to clear): {}  (Enter: confirm, Esc: cancel)",
                self.input_buffer
            ),
            InputMode::DateTo => format!(
                "to (YYYY-MM-DD, empty to clear): {}  (Enter: confirm, Esc: cancel)",
                self.input_buffer
            ),
        }
    }
}

/// # Errors
///
/// Returns an error if the terminal cannot be put into raw mode / alternate
/// screen, if the database cannot be read on startup, or if a terminal I/O
/// error occurs during the session.
pub fn run(storage: Storage) -> Result<(), TuiError> {
    let mut terminal = setup_terminal()?;
    let result = (|| -> Result<(), TuiError> {
        let mut app = App::new(storage)?;
        event_loop(&mut terminal, &mut app)
    })();
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, TuiError> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), TuiError> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<(), TuiError> {
    loop {
        terminal.draw(|f| ui(f, app))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if handle_key(app, key) {
                return Ok(());
            }
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return true;
    }
    match app.input_mode {
        InputMode::Normal => handle_normal_key(app, key),
        InputMode::Search | InputMode::DateFrom | InputMode::DateTo => handle_input_key(app, key),
    }
}

fn handle_normal_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') => true,
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_cursor(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_cursor(-1);
            false
        }
        KeyCode::Char('/') => {
            app.input_mode = InputMode::Search;
            app.input_buffer = app.search.clone();
            false
        }
        KeyCode::Char('f') => {
            app.input_mode = InputMode::DateFrom;
            app.input_buffer = app.date_from.map(format_date).unwrap_or_default();
            false
        }
        KeyCode::Char('t') => {
            app.input_mode = InputMode::DateTo;
            app.input_buffer = app.date_to.map(format_date).unwrap_or_default();
            false
        }
        KeyCode::Char('c') => {
            app.search.clear();
            app.date_from = None;
            app.date_to = None;
            app.reset_cursor();
            app.status = "filters cleared".to_string();
            false
        }
        KeyCode::Char('r') => {
            app.reload();
            false
        }
        KeyCode::Enter => {
            app.drill_in();
            false
        }
        KeyCode::Esc => {
            app.drill_out();
            false
        }
        _ => false,
    }
}

fn handle_input_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => {
            let buffer = app.input_buffer.clone();
            let mode = app.input_mode;
            app.input_mode = InputMode::Normal;
            app.input_buffer.clear();
            match mode {
                InputMode::Search => {
                    app.search = buffer;
                }
                InputMode::DateFrom => {
                    app.date_from = parse_date_input(&buffer);
                }
                InputMode::DateTo => {
                    app.date_to = parse_date_input(&buffer);
                }
                InputMode::Normal => unreachable!(),
            }
            app.reset_cursor();
            false
        }
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            app.input_buffer.clear();
            false
        }
        KeyCode::Backspace => {
            app.input_buffer.pop();
            false
        }
        KeyCode::Char(c) => {
            app.input_buffer.push(c);
            false
        }
        _ => false,
    }
}

fn format_date(t: Timestamp) -> String {
    t.to_zoned(TimeZone::UTC).date().to_string()
}

fn parse_date_input(s: &str) -> Option<Timestamp> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let date = Date::strptime("%Y-%m-%d", trimmed).ok()?;
    date.at(0, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp())
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    let title = match &app.view {
        View::Accounts => "ledger".to_string(),
        View::Register { account } => format!("ledger > {account}"),
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );

    let table = match &app.view {
        View::Accounts => {
            let header = Row::new(vec![
                Cell::from("account"),
                Cell::from(Text::from("balance").right_aligned()),
            ])
            .style(Style::default().add_modifier(Modifier::BOLD));
            let rows: Vec<Row> = app
                .filtered_accounts()
                .iter()
                .map(|(a, b)| {
                    Row::new(vec![
                        Cell::from(a.id.to_string()),
                        Cell::from(Text::from(format!("{b:.2}")).right_aligned()),
                    ])
                })
                .collect();
            Table::new(rows, [Constraint::Length(32), Constraint::Length(14)])
                .header(header)
                .column_spacing(2)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> ")
        }
        View::Register { account } => {
            let header = Row::new(vec![
                Cell::from("date"),
                Cell::from("memo"),
                Cell::from(Text::from("amount").right_aligned()),
            ])
            .style(Style::default().add_modifier(Modifier::BOLD));
            let rows: Vec<Row> = app
                .filtered_transactions()
                .iter()
                .map(|tx| {
                    let amount = tx
                        .entries
                        .iter()
                        .find(|e| &e.account == account)
                        .map_or(Decimal::ZERO, |e| e.amount);
                    Row::new(vec![
                        Cell::from(format_date(tx.posted_at)),
                        Cell::from(tx.memo.clone()),
                        Cell::from(Text::from(format!("{amount:.2}")).right_aligned()),
                    ])
                })
                .collect();
            Table::new(
                rows,
                [
                    Constraint::Length(12),
                    Constraint::Min(20),
                    Constraint::Length(14),
                ],
            )
            .header(header)
            .column_spacing(2)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ")
        }
    };
    f.render_stateful_widget(table, chunks[1], &mut app.table_state);

    let status = app.status_line();
    f.render_widget(Paragraph::new(status), chunks[2]);
}
