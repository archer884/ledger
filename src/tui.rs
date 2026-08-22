use std::collections::HashMap;
use std::io::{self, Stdout};
use std::str::FromStr;

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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
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
    AddTransaction,
    AddConfirm,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddField {
    From,
    To,
    Amount,
    Memo,
}

impl AddField {
    fn next(self) -> Self {
        match self {
            Self::From => Self::To,
            Self::To => Self::Amount,
            Self::Amount => Self::Memo,
            Self::Memo => Self::From,
        }
    }
}

const ADD_LIST_VISIBLE: usize = 7;

#[derive(Debug, Clone)]
struct AddTransactionState {
    accounts: Vec<AccountId>,
    from_index: usize,
    to_index: usize,
    from_offset: usize,
    to_offset: usize,
    amount: String,
    memo: String,
    focus: AddField,
    error: Option<String>,
    entries: Vec<Entry>,
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
    add_transaction: Option<AddTransactionState>,
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
            add_transaction: None,
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

    fn start_add(&mut self) {
        let accounts: Vec<AccountId> = self
            .accounts_with_balances
            .iter()
            .map(|(a, _)| a.id.clone())
            .collect();
        let to_index = accounts.len().min(2).saturating_sub(1);
        self.add_transaction = Some(AddTransactionState {
            accounts,
            from_index: 0,
            to_index,
            from_offset: 0,
            to_offset: 0,
            amount: String::new(),
            memo: String::new(),
            focus: AddField::From,
            error: None,
            entries: Vec::new(),
        });
        self.input_mode = InputMode::AddTransaction;
    }

    fn preview_add(&mut self) {
        let Some(state) = self.add_transaction.as_mut() else {
            self.input_mode = InputMode::Normal;
            return;
        };
        if state.accounts.len() < 2 {
            state.error = Some("need at least two registered accounts".to_string());
            return;
        }
        let (Some(from), Some(to)) = (
            state.accounts.get(state.from_index),
            state.accounts.get(state.to_index),
        ) else {
            state.error = Some("pick an account in both lists".to_string());
            return;
        };
        match build_add_entries(from, to, &state.amount) {
            Ok(entries) => {
                state.entries = entries;
                state.error = None;
                self.input_mode = InputMode::AddConfirm;
            }
            Err(e) => {
                state.error = Some(e);
            }
        }
    }

    fn confirm_add(&mut self) {
        let Some(state) = self.add_transaction.take() else {
            self.input_mode = InputMode::Normal;
            return;
        };
        let tx = match Transaction::new(
            TransactionId::new(),
            today_timestamp(),
            state.memo,
            state.entries,
        ) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("invalid transaction: {e}");
                self.input_mode = InputMode::Normal;
                return;
            }
        };
        match self.storage.save_transaction(&tx) {
            Ok(()) => {
                self.reload_after("transaction added");
            }
            Err(e) => {
                self.status = format!("add failed: {e}");
                self.input_mode = InputMode::Normal;
            }
        }
    }

    fn class_of(&self, id: &AccountId) -> Option<AccountClass> {
        self.accounts_with_balances
            .iter()
            .find(|(a, _)| &a.id == id)
            .map(|(a, _)| a.class)
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

    fn status_line(&self) -> Line<'static> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        match self.input_mode {
            InputMode::Normal => {
                let mut spans: Vec<Span<'static>> = Vec::new();
                if !self.search.is_empty() {
                    spans.push(Span::raw(format!("/{} ", self.search)));
                }
                if let Some(from) = self.date_from {
                    spans.push(Span::raw(format!("from:{} ", format_date(from))));
                }
                if let Some(to) = self.date_to {
                    spans.push(Span::raw(format!("to:{} ", format_date(to))));
                }
                if !self.status.is_empty() {
                    spans.push(Span::raw(format!("[{}] ", self.status)));
                }
                spans.push(Span::styled("?: help  q: quit", dim));
                Line::from(spans)
            }
            InputMode::Search => Line::raw(format!(
                "/{}  (Enter: confirm, Esc: cancel)",
                self.input_buffer
            )),
            InputMode::DateFrom => Line::raw(format!(
                "from (YYYY-MM-DD, empty to clear): {}  (Enter: confirm, Esc: cancel)",
                self.input_buffer
            )),
            InputMode::DateTo => Line::raw(format!(
                "to (YYYY-MM-DD, empty to clear): {}  (Enter: confirm, Esc: cancel)",
                self.input_buffer
            )),
            InputMode::DeleteConfirm => Line::raw("Delete this transaction? (y/n)"),
            InputMode::EditAccounts => Line::raw(
                "C: edit accounts  j/k: select entry  type to edit account  Enter: confirm  Esc: cancel",
            ),
            InputMode::EditAccountsConfirm => Line::raw("Apply account changes? (y/n)"),
            InputMode::AddTransaction => {
                Line::raw("a: add  Tab: next field  j/k: pick account  Enter: preview  Esc: cancel")
            }
            InputMode::AddConfirm => Line::raw("Save this transaction? (y/n)"),
            InputMode::Reconstruct | InputMode::Help => Line::raw("any key to close"),
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
        InputMode::AddTransaction => handle_add_transaction_key(app, key),
        InputMode::AddConfirm => handle_add_confirm_key(app, key),
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
        KeyCode::Char('a') => {
            app.start_add();
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
                | InputMode::AddTransaction
                | InputMode::AddConfirm
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

fn handle_add_transaction_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(state) = app.add_transaction.as_mut() else {
        app.input_mode = InputMode::Normal;
        return false;
    };
    let list_len = state.accounts.len();
    match key.code {
        KeyCode::Tab => {
            state.focus = state.focus.next();
            state.error = None;
            false
        }
        KeyCode::Enter => match state.focus {
            AddField::Memo => {
                app.preview_add();
                false
            }
            other => {
                state.focus = other.next();
                state.error = None;
                false
            }
        },
        KeyCode::Char('j') | KeyCode::Down
            if matches!(state.focus, AddField::From | AddField::To) =>
        {
            if list_len == 0 {
                return false;
            }
            match state.focus {
                AddField::From => {
                    state.from_index = (state.from_index + 1) % list_len;
                    state.from_offset =
                        clamp_offset(state.from_index, state.from_offset, ADD_LIST_VISIBLE);
                }
                AddField::To => {
                    state.to_index = (state.to_index + 1) % list_len;
                    state.to_offset =
                        clamp_offset(state.to_index, state.to_offset, ADD_LIST_VISIBLE);
                }
                _ => {}
            }
            false
        }
        KeyCode::Char('k') | KeyCode::Up
            if matches!(state.focus, AddField::From | AddField::To) =>
        {
            if list_len == 0 {
                return false;
            }
            match state.focus {
                AddField::From => {
                    state.from_index = state.from_index.checked_sub(1).unwrap_or(list_len - 1);
                    state.from_offset =
                        clamp_offset(state.from_index, state.from_offset, ADD_LIST_VISIBLE);
                }
                AddField::To => {
                    state.to_index = state.to_index.checked_sub(1).unwrap_or(list_len - 1);
                    state.to_offset =
                        clamp_offset(state.to_index, state.to_offset, ADD_LIST_VISIBLE);
                }
                _ => {}
            }
            false
        }
        KeyCode::Backspace => {
            state.error = None;
            match state.focus {
                AddField::Amount => {
                    state.amount.pop();
                }
                AddField::Memo => {
                    state.memo.pop();
                }
                AddField::From | AddField::To => {}
            }
            false
        }
        KeyCode::Esc => {
            app.add_transaction = None;
            app.input_mode = InputMode::Normal;
            app.status = "add cancelled".to_string();
            false
        }
        KeyCode::Char(c) if matches!(state.focus, AddField::Amount | AddField::Memo) => {
            state.error = None;
            match state.focus {
                AddField::Amount => state.amount.push(c),
                AddField::Memo => state.memo.push(c),
                AddField::From | AddField::To => {}
            }
            false
        }
        _ => false,
    }
}

fn handle_add_confirm_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y' | 'Y') => {
            app.confirm_add();
            false
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            app.input_mode = InputMode::AddTransaction;
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

/// Insert thousands separators into the integer part of an already
/// formatted decimal string, preserving any leading sign.
fn group_money(s: &str) -> String {
    let (int_part, rest) = match s.split_once('.') {
        Some((i, r)) => (i, format!(".{r}")),
        None => (s, String::new()),
    };
    let (sign, digits) = if let Some(d) = int_part.strip_prefix('-') {
        ("-", d)
    } else if let Some(d) = int_part.strip_prefix('+') {
        ("+", d)
    } else {
        ("", int_part)
    };
    let mut grouped = String::with_capacity(s.len() + digits.len() / 3);
    let n = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}{grouped}{rest}")
}

fn fmt_money(d: Decimal) -> String {
    group_money(&format!("{d:.2}"))
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

fn today_timestamp() -> Timestamp {
    jiff::Zoned::now()
        .date()
        .at(0, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .expect("midnight UTC is never ambiguous")
        .timestamp()
}

/// Build the two entries for the TUI add form: `-amount` on the `from`
/// account (the source) and `+amount` on the `to` account (the
/// destination). The accounts must differ and the amount must parse as
/// a nonzero `Decimal`.
fn build_add_entries(from: &AccountId, to: &AccountId, amount: &str) -> Result<Vec<Entry>, String> {
    if from == to {
        return Err("from and to accounts must differ".to_string());
    }
    let amount =
        Decimal::from_str(amount.trim()).map_err(|e| format!("invalid amount {amount:?}: {e}"))?;
    if amount == Decimal::ZERO {
        return Err("amount must be nonzero".to_string());
    }
    Ok(vec![
        Entry {
            account: from.clone(),
            amount: -amount,
        },
        Entry {
            account: to.clone(),
            amount,
        },
    ])
}

/// Keep a list window scrolled so `selected` stays visible within
/// `visible` rows starting at `offset`.
#[must_use]
fn clamp_offset(selected: usize, offset: usize, visible: usize) -> usize {
    if selected < offset {
        selected
    } else if selected >= offset + visible {
        selected + 1 - visible
    } else {
        offset
    }
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

    let empty_msg: Option<String> = match &app.view {
        View::Accounts => {
            if app.filtered_accounts().is_empty() {
                Some(if app.accounts_with_balances.is_empty() {
                    "no accounts yet — register one with `ledger add`".to_string()
                } else {
                    "no accounts match the current filters".to_string()
                })
            } else {
                None
            }
        }
        View::Register { account } => {
            if app.filtered_transactions().is_empty() {
                let has_any = app
                    .transactions
                    .iter()
                    .any(|t| t.entries.iter().any(|e| &e.account == account));
                Some(if has_any {
                    "no transactions match the current filters".to_string()
                } else {
                    "no transactions yet for this account".to_string()
                })
            } else {
                None
            }
        }
    };

    if let Some(msg) = empty_msg {
        render_empty_state(f, chunks[1], &msg);
    } else {
        let table = match &app.view {
            View::Accounts => accounts_table(app),
            View::Register { account } => register_table(app, account),
        };
        f.render_stateful_widget(table, chunks[1], &mut app.table_state);
    }

    let status = app.status_line();
    f.render_widget(Paragraph::new(status), chunks[2]);

    match app.input_mode {
        InputMode::DeleteConfirm => {
            render_delete_confirm(f);
        }
        InputMode::EditAccounts | InputMode::EditAccountsConfirm => {
            render_edit_accounts_modal(f, app);
        }
        InputMode::AddTransaction | InputMode::AddConfirm => {
            render_add_modal(f, app);
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

fn accounts_table(app: &App) -> Table<'static> {
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
                Cell::from(Text::from(fmt_money(*b)).right_aligned()),
            ])
        })
        .collect();
    Table::new(rows, [Constraint::Length(32), Constraint::Length(14)])
        .header(header)
        .column_spacing(2)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ")
}

fn register_table(app: &App, account: &AccountId) -> Table<'static> {
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
                Cell::from(Text::from(fmt_money(amount)).right_aligned()),
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
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ")
}

fn render_empty_state(f: &mut ratatui::Frame, area: Rect, msg: &str) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    let text = Line::raw(msg.to_string()).style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(
        Paragraph::new(text).alignment(Alignment::Center),
        vertical[1],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(height) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(area.width.saturating_sub(width) / 2),
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

fn render_add_modal(f: &mut ratatui::Frame, app: &App) {
    let Some(state) = &app.add_transaction else {
        return;
    };
    if app.input_mode == InputMode::AddConfirm {
        render_add_confirm_modal(f, app, state);
    } else {
        render_add_form_modal(f, state);
    }
}

fn render_add_confirm_modal(f: &mut ratatui::Frame, app: &App, state: &AddTransactionState) {
    let mut content = Vec::new();
    for entry in &state.entries {
        let class = app
            .class_of(&entry.account)
            .map_or_else(String::new, |c| format!(" ({c:?})"))
            .to_lowercase();
        content.push(Line::from(vec![
            Span::raw(format!("  {:<24}", entry.account.to_string())),
            Span::raw(format!(
                "{:>12}{class}",
                group_money(&format!("{:+.2}", entry.amount))
            )),
        ]));
    }
    if !state.memo.is_empty() {
        content.push(Line::raw(format!("  memo: {}", state.memo)));
    }
    content.push(Line::raw(""));
    content.push(
        Line::raw(format!(
            "  Posts today ({})",
            format_date(today_timestamp())
        ))
        .style(Style::default().add_modifier(Modifier::DIM)),
    );
    let area = centered_rect(58, 8, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("Add transaction — Save? (y/n)");
    let paragraph = Paragraph::new(content).block(block);
    f.render_widget(paragraph, area);
}

fn render_add_form_modal(f: &mut ratatui::Frame, state: &AddTransactionState) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let reversed = Style::default().add_modifier(Modifier::REVERSED);

    let from_focused = state.focus == AddField::From;
    let to_focused = state.focus == AddField::To;

    let header = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{:<28}", "FROM (-)"),
            if from_focused { reversed } else { bold },
        ),
        Span::raw("  "),
        Span::styled(
            format!("{:<28}", "TO (+)"),
            if to_focused { reversed } else { bold },
        ),
    ]);

    let pane_rows =
        |ids: &[AccountId], selected: usize, offset: usize, focused: bool| -> Vec<(String, bool)> {
            let end = (offset + ADD_LIST_VISIBLE).min(ids.len());
            (offset..end)
                .map(|i| {
                    let marker = if i == selected { "> " } else { "  " };
                    (
                        format!("{marker}{:<26.26}", ids[i].as_str()),
                        i == selected && focused,
                    )
                })
                .collect()
        };

    let left_rows = pane_rows(
        &state.accounts,
        state.from_index,
        state.from_offset,
        from_focused,
    );
    let right_rows = pane_rows(&state.accounts, state.to_index, state.to_offset, to_focused);

    let mut content = vec![header];
    for r in 0..ADD_LIST_VISIBLE {
        let (left, left_hot) = left_rows
            .get(r)
            .map_or((String::new(), false), |(s, hot)| ((*s).clone(), *hot));
        let (right, right_hot) = right_rows
            .get(r)
            .map_or((String::new(), false), |(s, hot)| ((*s).clone(), *hot));
        content.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{left:<28}"), if left_hot { reversed } else { dim }),
            Span::raw("  "),
            Span::styled(
                format!("{right:<28}"),
                if right_hot { reversed } else { dim },
            ),
        ]));
    }

    let summary = add_summary_line(state, bold, dim);
    content.push(summary);
    content.push(Line::raw(""));

    let field = |label: &str, buffer: &str| -> Line {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{label:<10}"),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::raw(buffer.to_string()),
        ])
    };

    content.push(field("amount", &state.amount));
    content.push(field("memo", &state.memo));
    content.push(Line::raw(""));

    if let Some(error) = &state.error {
        content.push(Line::raw(format!("  {error}")));
    } else {
        content.push(Line::raw(""));
    }
    content.push(
        Line::raw("  Tab: next field  j/k: pick account  Enter: preview  Esc: cancel").style(dim),
    );

    let area = centered_rect(64, 17, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("Add transaction");
    let inner = block.inner(area);
    let paragraph = Paragraph::new(content).block(block);
    f.render_widget(paragraph, area);

    set_add_form_cursor(f, inner, state);
}

/// The `from → to` line showing the current selection under the panes.
fn add_summary_line(state: &AddTransactionState, bold: Style, dim: Style) -> Line<'static> {
    match (
        state.accounts.get(state.from_index),
        state.accounts.get(state.to_index),
    ) {
        (Some(from), Some(to)) => Line::from(vec![
            Span::raw("  "),
            Span::styled(from.as_str().to_string(), bold),
            Span::styled("  →  ", dim),
            Span::styled(to.as_str().to_string(), bold),
        ]),
        _ => Line::raw("  (no accounts)").style(dim),
    }
}

/// Put the terminal's own cursor right after the focused text so the
/// input looks native. Layout: row 0 is the pane header, rows 1 to
/// `ADD_LIST_VISIBLE` are the account lists, then the summary row,
/// a blank row, then the amount and memo fields. Text starts 2 cells
/// in plus the 10-wide label column.
fn set_add_form_cursor(f: &mut ratatui::Frame, inner: Rect, state: &AddTransactionState) {
    if !matches!(state.focus, AddField::Amount | AddField::Memo) {
        return;
    }
    // Typed characters are single-width and the row indices are tiny;
    // truncation from usize is not a real concern here.
    #[allow(clippy::cast_possible_truncation)]
    {
        let (buffer, row) = if state.focus == AddField::Amount {
            (&state.amount, ADD_LIST_VISIBLE + 3)
        } else {
            (&state.memo, ADD_LIST_VISIBLE + 4)
        };
        let typed_width = buffer.chars().count() as u16;
        let x = (inner.x + 12 + typed_width).min(inner.right().saturating_sub(1));
        f.set_cursor_position((x, inner.y + row as u16));
    }
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
        row("a", "add transaction (y/n to confirm)"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::Backend as _;

    fn acct_id(s: &str) -> AccountId {
        AccountId::parse(s).expect("account parses")
    }

    #[test]
    fn build_add_entries_builds_signed_pair() {
        let entries =
            build_add_entries(&acct_id("income/dues"), &acct_id("general"), "60").expect("ok");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].account.as_str(), "income/dues");
        assert_eq!(entries[0].amount, Decimal::from(-60));
        assert_eq!(entries[1].account.as_str(), "general");
        assert_eq!(entries[1].amount, Decimal::from(60));
        let sum: Decimal = entries.iter().map(|e| e.amount).sum();
        assert_eq!(sum, Decimal::ZERO);
    }

    #[test]
    fn build_add_entries_accepts_decimal_amounts() {
        let entries =
            build_add_entries(&acct_id("general"), &acct_id("expense/city"), "154.56").expect("ok");
        assert_eq!(entries[0].amount, Decimal::from_str("-154.56").unwrap());
        assert_eq!(entries[1].amount, Decimal::from_str("154.56").unwrap());
    }

    #[test]
    fn build_add_entries_rejects_same_account() {
        let err =
            build_add_entries(&acct_id("general"), &acct_id("general"), "60").expect_err("same");
        assert!(err.contains("must differ"), "{err}");
    }

    #[test]
    fn build_add_entries_rejects_bad_or_zero_amount() {
        let from = acct_id("general");
        let to = acct_id("income/dues");
        let err = build_add_entries(&from, &to, "abc").expect_err("bad");
        assert!(err.contains("invalid amount"), "{err}");
        let err = build_add_entries(&from, &to, "0").expect_err("zero");
        assert!(err.contains("nonzero"), "{err}");
    }

    #[test]
    fn clamp_offset_follows_selection() {
        assert_eq!(clamp_offset(2, 5, 7), 2);
        assert_eq!(clamp_offset(9, 0, 7), 3);
        assert_eq!(clamp_offset(4, 3, 7), 3);
    }

    #[test]
    fn money_formatting_groups_thousands() {
        assert_eq!(fmt_money(Decimal::ZERO), "0.00");
        assert_eq!(fmt_money(Decimal::from(60)), "60.00");
        assert_eq!(fmt_money(Decimal::from_str("1543.51").unwrap()), "1,543.51");
        assert_eq!(
            fmt_money(Decimal::from_str("-1234567.89").unwrap()),
            "-1,234,567.89"
        );
        assert_eq!(group_money("+1234567.89"), "+1,234,567.89");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            handle_key(app, key(KeyCode::Char(c)));
        }
    }

    fn seeded_app() -> App {
        let storage = Storage::in_memory().expect("in-memory db");
        for (id, class) in [
            ("general", AccountClass::Asset),
            ("income/dues", AccountClass::Income),
            ("expense/city", AccountClass::Expense),
        ] {
            storage
                .register_account(&Account {
                    id: AccountId::parse(id).expect("account parses"),
                    class,
                })
                .expect("registered");
        }
        App::new(storage).expect("app loads")
    }

    #[test]
    fn add_flow_end_to_end() {
        let mut app = seeded_app();

        handle_key(&mut app, key(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::AddTransaction);

        // accounts list is sorted: expense/city(0), general(1), income/dues(2)
        handle_key(&mut app, key(KeyCode::Char('j')));
        handle_key(&mut app, key(KeyCode::Char('j')));
        let state = app.add_transaction.as_ref().expect("state");
        assert_eq!(state.from_index, 2);
        assert_eq!(state.accounts[state.from_index].as_str(), "income/dues");

        handle_key(&mut app, key(KeyCode::Tab));
        let state = app.add_transaction.as_ref().expect("state");
        assert_eq!(state.to_index, 1);
        assert_eq!(state.accounts[state.to_index].as_str(), "general");

        handle_key(&mut app, key(KeyCode::Tab));
        type_str(&mut app, "60");
        handle_key(&mut app, key(KeyCode::Tab));
        type_str(&mut app, "Dues deposit");
        handle_key(&mut app, key(KeyCode::Enter));

        assert_eq!(app.input_mode, InputMode::AddConfirm);
        let state = app.add_transaction.as_ref().expect("state kept");
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].account.as_str(), "income/dues");
        assert_eq!(state.entries[0].amount, Decimal::from(-60));
        assert_eq!(state.entries[1].account.as_str(), "general");
        assert_eq!(state.entries[1].amount, Decimal::from(60));

        handle_key(&mut app, key(KeyCode::Char('y')));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.status, "transaction added");

        let transactions = app.storage.list_transactions().expect("list works");
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].memo, "Dues deposit");
        let sum: Decimal = transactions[0].entries.iter().map(|e| e.amount).sum();
        assert_eq!(sum, Decimal::ZERO);
    }

    #[test]
    fn add_flow_decline_returns_to_form_and_escape_cancels() {
        let mut app = seeded_app();

        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Tab));
        handle_key(&mut app, key(KeyCode::Tab));
        type_str(&mut app, "60");
        handle_key(&mut app, key(KeyCode::Tab));
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::AddConfirm);

        handle_key(&mut app, key(KeyCode::Char('n')));
        assert_eq!(app.input_mode, InputMode::AddTransaction);

        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(
            app.storage
                .list_transactions()
                .expect("list works")
                .is_empty()
        );
    }

    #[test]
    fn add_flow_enter_on_from_list_advances_focus() {
        let mut app = seeded_app();
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Enter));
        let state = app.add_transaction.as_ref().expect("state");
        assert_eq!(state.focus, AddField::To);
        assert_eq!(app.input_mode, InputMode::AddTransaction);
    }

    #[test]
    fn add_flow_jk_type_in_text_fields_not_lists() {
        let mut app = seeded_app();
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Tab));
        handle_key(&mut app, key(KeyCode::Tab));
        type_str(&mut app, "j60k");
        let state = app.add_transaction.as_ref().expect("state");
        assert_eq!(state.amount, "j60k");
    }

    #[test]
    fn add_flow_validation_error_keeps_form_open() {
        let mut app = seeded_app();
        handle_key(&mut app, key(KeyCode::Char('a')));
        // from defaults to index 0; select index 0 on the TO list too
        handle_key(&mut app, key(KeyCode::Tab));
        handle_key(&mut app, key(KeyCode::Char('k')));
        handle_key(&mut app, key(KeyCode::Tab));
        type_str(&mut app, "60");
        handle_key(&mut app, key(KeyCode::Tab));
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::AddTransaction);
        let state = app.add_transaction.as_ref().expect("state");
        let error = state.error.as_deref().expect("error set");
        assert!(error.contains("must differ"), "{error}");
    }

    fn buffer_text(terminal: &Terminal<ratatui::backend::TestBackend>) -> String {
        let mut text = String::new();
        for cell in terminal.backend().buffer().content() {
            text.push(cell.symbol().chars().next().unwrap_or(' '));
        }
        text
    }

    #[test]
    fn render_add_modal_smoke() {
        let mut app = seeded_app();
        handle_key(&mut app, key(KeyCode::Char('a')));

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| ui(f, &mut app)).expect("renders");
        let text = buffer_text(&terminal);
        assert!(text.contains("Add transaction"), "title missing");
        assert!(text.contains("FROM"), "FROM pane missing");
        assert!(text.contains("TO"), "TO pane missing");
        assert!(text.contains("amount"), "amount field missing");
        assert!(text.contains("memo"), "memo field missing");
        assert!(text.contains("income/dues"), "account list missing");

        handle_key(&mut app, key(KeyCode::Char('j')));
        handle_key(&mut app, key(KeyCode::Char('j')));
        handle_key(&mut app, key(KeyCode::Tab));
        handle_key(&mut app, key(KeyCode::Tab));
        type_str(&mut app, "60");
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| ui(f, &mut app)).expect("renders");
        let text = buffer_text(&terminal);
        assert!(!text.contains('|'), "fake cursor char leaked: {text}");

        // Terminal cursor sits right after "60" in the amount field:
        // modal inner starts at x=9 (80/64 centering + border), text at
        // +12 (indent + label column), +2 typed chars; row 9 of the
        // inner area (header + 7 list rows + blank).
        let pos = terminal
            .backend_mut()
            .get_cursor_position()
            .expect("cursor position set");
        assert_eq!((pos.x, pos.y), (23, 14), "cursor not after amount text");

        handle_key(&mut app, key(KeyCode::Tab));
        type_str(&mut app, "Dues");
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::AddConfirm);

        terminal.draw(|f| ui(f, &mut app)).expect("renders");
        let text = buffer_text(&terminal);
        assert!(
            text.contains("memo: Dues"),
            "memo missing in preview: {text}"
        );
        assert!(
            text.contains("(income)"),
            "class suffix truncated in preview: {text}"
        );
        assert!(
            text.contains("Posts today ("),
            "dated footer missing in preview: {text}"
        );
    }

    #[test]
    fn render_empty_states() {
        let mut app = seeded_app();
        app.search = "zzz".to_string();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| ui(f, &mut app)).expect("renders");
        let text = buffer_text(&terminal);
        assert!(
            text.contains("no accounts match the current filters"),
            "accounts empty state missing: {text}"
        );
    }

    #[test]
    fn render_add_modal_survives_tiny_terminal() {
        let mut app = seeded_app();
        handle_key(&mut app, key(KeyCode::Char('a')));

        let backend = ratatui::backend::TestBackend::new(1, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| ui(f, &mut app))
            .expect("no underflow on a zero-sized modal area");
    }
}
