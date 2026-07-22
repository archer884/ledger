use std::collections::HashMap;
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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState,
};
use rust_decimal::Decimal;
use thiserror::Error;

use crate::model::{Account, AccountClass, AccountId, Entry, Transaction, TransactionId};
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
    DeleteConfirm,
    EditAccounts,
    EditAccountsConfirm,
    Reconstruct,
    Help,
}

#[derive(Debug, Clone)]
struct EditAccountsState {
    id: TransactionId,
    posted_at: Timestamp,
    memo: String,
    entries: Vec<Entry>,
    buffers: Vec<String>,
    selected: usize,
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
    edit_accounts: Option<EditAccountsState>,
    reconstruct_cmd: Option<String>,
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
            edit_accounts: None,
            reconstruct_cmd: None,
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

    fn selected_transaction(&self) -> Option<Transaction> {
        if !matches!(self.view, View::Register { .. }) {
            return None;
        }
        let idx = self.table_state.selected()?;
        self.filtered_transactions().get(idx).cloned()
    }

    fn start_delete(&mut self) {
        if self.selected_transaction().is_some() {
            self.input_mode = InputMode::DeleteConfirm;
        }
    }

    fn confirm_delete(&mut self) {
        let Some(tx) = self.selected_transaction() else {
            self.input_mode = InputMode::Normal;
            return;
        };
        match self.storage.delete_transaction(tx.id) {
            Ok(()) => {
                self.reload_after("transaction deleted");
            }
            Err(e) => {
                self.status = format!("delete failed: {e}");
                self.input_mode = InputMode::Normal;
            }
        }
    }

    fn start_edit_accounts(&mut self) {
        let Some(tx) = self.selected_transaction() else {
            return;
        };
        let buffers = tx.entries.iter().map(|e| e.account.to_string()).collect();
        self.edit_accounts = Some(EditAccountsState {
            id: tx.id,
            posted_at: tx.posted_at,
            memo: tx.memo.clone(),
            entries: tx.entries.clone(),
            buffers,
            selected: 0,
        });
        self.input_mode = InputMode::EditAccounts;
    }

    fn confirm_edit_accounts(&mut self) {
        let Some(state) = self.edit_accounts.take() else {
            self.input_mode = InputMode::Normal;
            return;
        };
        let mut entries = Vec::with_capacity(state.entries.len());
        for (i, original) in state.entries.iter().enumerate() {
            let buffer = state.buffers.get(i).map_or("", String::as_str);
            let account = match AccountId::parse(buffer) {
                Ok(id) => id,
                Err(e) => {
                    self.status = format!("invalid account {buffer:?}: {e}");
                    self.edit_accounts = Some(state);
                    self.input_mode = InputMode::EditAccounts;
                    return;
                }
            };
            entries.push(Entry {
                account,
                amount: original.amount,
            });
        }
        let tx = match Transaction::new(state.id, state.posted_at, state.memo.clone(), entries) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("invalid transaction: {e}");
                self.edit_accounts = Some(state);
                self.input_mode = InputMode::EditAccounts;
                return;
            }
        };
        match self.storage.replace_transaction(&tx) {
            Ok(()) => {
                self.reload_after("accounts updated");
            }
            Err(e) => {
                self.status = format!("update failed: {e}");
                self.input_mode = InputMode::Normal;
            }
        }
    }

    fn start_reconstruct(&mut self) {
        let Some(tx) = self.selected_transaction() else {
            return;
        };
        let classes: HashMap<AccountId, AccountClass> = self
            .accounts_with_balances
            .iter()
            .map(|(a, _)| (a.id.clone(), a.class))
            .collect();
        let cmd = match tx.render_add_command(&classes) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("reconstruct failed: {e}");
                return;
            }
        };
        match arboard::Clipboard::new()
            .and_then(|mut cb| cb.set_text(cmd.trim_end_matches('\n').to_string()))
        {
            Ok(()) => {
                self.status = "command copied to clipboard".to_string();
            }
            Err(e) => {
                self.status = format!("clipboard unavailable ({e}); showing command");
                self.reconstruct_cmd = Some(cmd);
                self.input_mode = InputMode::Reconstruct;
            }
        }
    }

    fn close_reconstruct(&mut self) {
        self.reconstruct_cmd = None;
        self.input_mode = InputMode::Normal;
    }

    fn reload_after(&mut self, status: &str) {
        if let Err(e) = self.load() {
            self.status = format!("reload failed: {e}");
        } else {
            self.status = status.to_string();
        }
        self.input_mode = InputMode::Normal;
        self.reset_cursor();
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
                s.push_str("?: help  q: quit");
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
            InputMode::DeleteConfirm => "Delete this transaction? (y/n)".to_string(),
            InputMode::EditAccounts => "C: edit accounts  j/k: select entry  type to edit account  Enter: confirm  Esc: cancel".to_string(),
            InputMode::EditAccountsConfirm => "Apply account changes? (y/n)".to_string(),
            InputMode::Reconstruct | InputMode::Help => "any key to close".to_string(),
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
        InputMode::DeleteConfirm => handle_delete_confirm_key(app, key),
        InputMode::EditAccounts => handle_edit_accounts_key(app, key),
        InputMode::EditAccountsConfirm => handle_edit_accounts_confirm_key(app, key),
        InputMode::Reconstruct => handle_reconstruct_key(app, key),
        InputMode::Help => handle_help_key(app, key),
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
        KeyCode::Char('D') => {
            app.start_delete();
            false
        }
        KeyCode::Char('C') => {
            app.start_edit_accounts();
            false
        }
        KeyCode::Char('y') => {
            app.start_reconstruct();
            false
        }
        KeyCode::Char('?') => {
            app.input_mode = InputMode::Help;
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
                InputMode::Normal
                | InputMode::DeleteConfirm
                | InputMode::EditAccounts
                | InputMode::EditAccountsConfirm
                | InputMode::Reconstruct
                | InputMode::Help => unreachable!(),
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

fn handle_delete_confirm_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y' | 'Y') => {
            app.confirm_delete();
            false
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => {
            app.input_mode = InputMode::Normal;
            app.status = "delete cancelled".to_string();
            false
        }
        _ => false,
    }
}

fn handle_edit_accounts_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(state) = app.edit_accounts.as_mut() else {
        app.input_mode = InputMode::Normal;
        return false;
    };
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if state.entries.is_empty() {
                return false;
            }
            let next = (state.selected + 1) % state.entries.len();
            state.selected = next;
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if state.entries.is_empty() {
                return false;
            }
            let prev = state
                .selected
                .checked_sub(1)
                .unwrap_or(state.entries.len() - 1);
            state.selected = prev;
            false
        }
        KeyCode::Backspace => {
            if let Some(buf) = state.buffers.get_mut(state.selected) {
                buf.pop();
            }
            false
        }
        KeyCode::Enter => {
            app.input_mode = InputMode::EditAccountsConfirm;
            false
        }
        KeyCode::Esc => {
            app.edit_accounts = None;
            app.input_mode = InputMode::Normal;
            app.status = "edit cancelled".to_string();
            false
        }
        KeyCode::Char(c) => {
            if let Some(buf) = state.buffers.get_mut(state.selected) {
                buf.push(c);
            }
            false
        }
        _ => false,
    }
}

fn handle_edit_accounts_confirm_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y' | 'Y') => {
            app.confirm_edit_accounts();
            false
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            app.input_mode = InputMode::EditAccounts;
            app.status = "edit not applied".to_string();
            false
        }
        _ => false,
    }
}

fn handle_reconstruct_key(app: &mut App, _key: KeyEvent) -> bool {
    app.close_reconstruct();
    false
}

fn handle_help_key(app: &mut App, _key: KeyEvent) -> bool {
    app.input_mode = InputMode::Normal;
    false
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

    match app.input_mode {
        InputMode::DeleteConfirm => {
            render_delete_confirm(f);
        }
        InputMode::EditAccounts | InputMode::EditAccountsConfirm => {
            render_edit_accounts_modal(f, app);
        }
        InputMode::Reconstruct => {
            render_reconstruct_modal(f, app);
        }
        InputMode::Help => {
            render_help_modal(f);
        }
        _ => {}
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height - height) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width - width) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(area);
    vertical[1].intersection(horizontal[1])
}

fn render_delete_confirm(f: &mut ratatui::Frame) {
    let area = centered_rect(38, 3, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("Delete transaction");
    let paragraph = Paragraph::new("Delete this transaction? (y/n)").block(block);
    f.render_widget(paragraph, area);
}

fn render_edit_accounts_modal(f: &mut ratatui::Frame, app: &App) {
    let Some(state) = &app.edit_accounts else {
        return;
    };

    let title = if app.input_mode == InputMode::EditAccountsConfirm {
        "Edit accounts — Apply changes? (y/n)"
    } else {
        "Edit accounts"
    };

    let width = 48;
    // Entry counts in a single transaction are tiny; truncation from a
    // usize->u16 is not a real concern here.
    #[allow(clippy::cast_possible_truncation)]
    let height = 3 + state.entries.len() as u16;
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title);

    let rows: Vec<Line> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == state.selected;
            let buf = state.buffers.get(i).map_or("", String::as_str);
            let account_label = format!("  {buf:<28}");
            let amount_label = format!("{:>14.2}", entry.amount);
            let line = Line::from(vec![
                Span::raw(account_label),
                Span::styled(amount_label, Style::default().add_modifier(Modifier::DIM)),
            ]);
            if selected {
                line.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();

    let help = "  j/k: select  Enter: confirm  Esc: cancel";

    let content: Vec<Line> = rows
        .into_iter()
        .chain(std::iter::once(Line::raw("  ")))
        .chain(std::iter::once(
            Line::raw(help).style(Style::default().add_modifier(Modifier::DIM)),
        ))
        .collect();

    let paragraph = Paragraph::new(content).block(block);
    f.render_widget(paragraph, area);
}

fn render_reconstruct_modal(f: &mut ratatui::Frame, app: &App) {
    let Some(cmd) = &app.reconstruct_cmd else {
        return;
    };
    let lines: Vec<Line> = cmd.lines().map(Line::raw).collect();
    // 2 for the top/bottom border, 1 for the trailing help. Lines are
    // already short enough to fit a typical terminal; clippy is satisfied
    // by capping at a sensible width.
    #[allow(clippy::cast_possible_truncation)]
    let height = (lines.len() as u16) + 3;
    let area = centered_rect(60, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("ledger add command (clipboard unavailable — any key to close)");
    let help = Line::raw("  copy from your terminal selection")
        .style(Style::default().add_modifier(Modifier::DIM));
    let content: Vec<Line> = lines
        .into_iter()
        .chain(std::iter::once(Line::raw("")))
        .chain(std::iter::once(help))
        .collect();
    let paragraph = Paragraph::new(content).block(block);
    f.render_widget(paragraph, area);
}

fn render_help_modal(f: &mut ratatui::Frame) {
    fn header(s: &str) -> Line<'static> {
        Line::from(s.to_string()).style(Style::default().add_modifier(Modifier::BOLD))
    }
    fn row(keys: &str, desc: &str) -> Line<'static> {
        Line::from(vec![
            Span::raw(format!("  {keys:<18}")),
            Span::raw(desc.to_string()),
        ])
    }
    let content: Vec<Line> = vec![
        header("Navigation"),
        row("j / k / \u{2191}\u{2193}", "move cursor"),
        row("Enter", "drill into account"),
        row("Esc", "back to accounts"),
        Line::raw(""),
        header("Search & filter"),
        row("/", "substring search"),
        row("f / t", "set from / to date filter"),
        row("c", "clear all filters"),
        Line::raw(""),
        header("Transaction"),
        row("y", "copy the ledger add command to the clipboard"),
        row("C", "edit accounts (y/n to apply)"),
        row("D", "delete (y/n to confirm)"),
        Line::raw(""),
        header("App"),
        row("r", "reload from disk"),
        row("?", "show this help"),
        row("q / Ctrl-C", "quit"),
    ];
    #[allow(clippy::cast_possible_truncation)]
    let height = (content.len() as u16) + 2;
    let area = centered_rect(48, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("Keyboard shortcuts (any key to close)");
    let paragraph = Paragraph::new(content).block(block);
    f.render_widget(paragraph, area);
}
