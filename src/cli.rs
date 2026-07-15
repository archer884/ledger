use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rust_decimal::Decimal;

use crate::model::{
    Account, AccountClass, AccountId, AccountIdError, Entry, Transaction, TransactionError,
    TransactionId,
};
use crate::storage::{Storage, StorageError};
use crate::tui::TuiError;

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// Path to the database file (overrides --book)
    #[arg(long, env = "LEDGER_DB", global = true)]
    pub db: Option<PathBuf>,

    /// Name for this set of books (e.g. "personal" -> ledger.personal.db)
    #[arg(long, alias = "ledger", env = "LEDGER_BOOK", global = true)]
    pub book: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Add a transaction to the ledger
    Add(AddArgs),

    /// Record income (debit checking, credit income)
    Income(IncomeArgs),

    /// Launch the TUI (the default if no subcommand is given)
    Tui(TuiArgs),
}

#[derive(Args)]
pub struct TuiArgs {}

#[derive(Args)]
pub struct AddArgs {
    /// Date of the transaction (YYYY-MM-DD), defaults to today
    #[arg(long)]
    pub date: Option<String>,

    /// Memo describing the transaction
    #[arg(long)]
    pub memo: Option<String>,

    /// Entry in the form ID[:CLASS]:AMOUNT (e.g. checking:asset:1000)
    #[arg(long = "entry", value_name = "ID[:CLASS]:AMOUNT")]
    pub entries: Vec<String>,
}

#[derive(Args)]
pub struct IncomeArgs {
    /// Amount of income
    #[arg(value_name = "AMOUNT")]
    pub amount: String,

    /// Date of the transaction (YYYY-MM-DD), defaults to today
    #[arg(long)]
    pub date: Option<String>,

    /// Memo describing the transaction
    #[arg(long)]
    pub memo: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum AccountClassArg {
    Asset,
    Liability,
    Equity,
    Expense,
}

impl From<AccountClassArg> for AccountClass {
    fn from(arg: AccountClassArg) -> Self {
        match arg {
            AccountClassArg::Asset => AccountClass::Asset,
            AccountClassArg::Liability => AccountClass::Liability,
            AccountClassArg::Equity => AccountClass::Equity,
            AccountClassArg::Expense => AccountClass::Expense,
        }
    }
}

#[derive(Debug)]
pub enum CliError {
    Storage(StorageError),
    Transaction(TransactionError),
    AccountId(AccountIdError),
    Entry(String),
    Amount(String),
    Date(String),
    BookName(String),
    Tui(TuiError),
    Io(std::io::Error),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "{e}"),
            Self::Transaction(e) => write!(f, "{e}"),
            Self::AccountId(e) => write!(f, "{e}"),
            Self::Entry(s) => write!(f, "invalid entry: {s} (expected ID[:CLASS]:AMOUNT)"),
            Self::Amount(s) => write!(f, "invalid amount: {s}"),
            Self::Date(s) => write!(f, "invalid date: {s}"),
            Self::BookName(s) => write!(
                f,
                "invalid book name: {s:?} (must contain only letters and numbers)"
            ),
            Self::Tui(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<StorageError> for CliError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}

impl From<TransactionError> for CliError {
    fn from(e: TransactionError) -> Self {
        Self::Transaction(e)
    }
}

impl From<AccountIdError> for CliError {
    fn from(e: AccountIdError) -> Self {
        Self::AccountId(e)
    }
}

impl From<TuiError> for CliError {
    fn from(e: TuiError) -> Self {
        Self::Tui(e)
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// # Errors
///
/// Returns an error if argument parsing fails, the database cannot be
/// opened, or the dispatched subcommand fails.
pub fn execute() -> Result<(), CliError> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Tui(TuiArgs {}));
    let storage = open_storage(cli.db.as_deref(), cli.book.as_deref())?;
    match command {
        Command::Add(args) => run_add(&args, &storage),
        Command::Income(args) => run_income(&args, &storage),
        Command::Tui(_args) => crate::tui::run(storage).map_err(Into::into),
    }
}

fn run_add(args: &AddArgs, storage: &Storage) -> Result<(), CliError> {
    execute_transaction(
        args.date.as_deref(),
        args.memo.as_deref(),
        &args.entries,
        storage,
    )
}

fn run_income(args: &IncomeArgs, storage: &Storage) -> Result<(), CliError> {
    let amount = Decimal::from_str(&args.amount).map_err(|e| CliError::Amount(format!("{e}")))?;
    let entries = vec![
        format!("checking:asset:{amount}"),
        format!("income:equity:-{amount}"),
    ];
    execute_transaction(
        args.date.as_deref(),
        args.memo.as_deref(),
        &entries,
        storage,
    )
}

fn execute_transaction(
    date: Option<&str>,
    memo: Option<&str>,
    entry_strs: &[String],
    storage: &Storage,
) -> Result<(), CliError> {
    let posted_at = resolve_date(date)?;
    let memo = memo.unwrap_or_default().to_string();

    let mut entries = Vec::with_capacity(entry_strs.len());
    for entry_str in entry_strs {
        let parsed = parse_entry(entry_str)?;
        if let Some(class) = parsed.class {
            storage.register_account(&Account {
                id: parsed.id.clone(),
                class: class.into(),
            })?;
        }
        entries.push(Entry {
            account: parsed.id,
            amount: parsed.amount,
        });
    }

    let tx = Transaction::new(TransactionId::new(), posted_at, memo, entries)?;

    storage.save_transaction(&tx)?;

    println!("added transaction {}", tx.id.0);
    Ok(())
}

fn resolve_date(s: Option<&str>) -> Result<Timestamp, CliError> {
    match s {
        None | Some("") => Ok(today()),
        Some(s) => parse_date(s),
    }
}

fn today() -> Timestamp {
    let zoned = jiff::Zoned::now();
    zoned
        .date()
        .at(0, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .expect("midnight UTC is never ambiguous")
        .timestamp()
}

fn parse_date(s: &str) -> Result<Timestamp, CliError> {
    let date = Date::strptime("%Y-%m-%d", s).map_err(|e| CliError::Date(format!("{s}: {e}")))?;
    let zoned = date
        .at(0, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .map_err(|e| CliError::Date(format!("{s}: {e}")))?;
    Ok(zoned.timestamp())
}

struct ParsedEntry {
    id: AccountId,
    class: Option<AccountClassArg>,
    amount: Decimal,
}

fn parse_entry(s: &str) -> Result<ParsedEntry, CliError> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 => {
            let id = AccountId::parse(parts[0])?;
            let amount = Decimal::from_str(parts[1])
                .map_err(|e| CliError::Entry(format!("{s}: invalid amount ({e})")))?;
            Ok(ParsedEntry {
                id,
                class: None,
                amount,
            })
        }
        3 => {
            let id = AccountId::parse(parts[0])?;
            let class = match parts[1] {
                "asset" => AccountClassArg::Asset,
                "liability" => AccountClassArg::Liability,
                "equity" => AccountClassArg::Equity,
                "expense" => AccountClassArg::Expense,
                other => {
                    return Err(CliError::Entry(format!("{s}: unknown class '{other}'")));
                }
            };
            let amount = Decimal::from_str(parts[2])
                .map_err(|e| CliError::Entry(format!("{s}: invalid amount ({e})")))?;
            Ok(ParsedEntry {
                id,
                class: Some(class),
                amount,
            })
        }
        _ => Err(CliError::Entry(s.to_string())),
    }
}

/// # Errors
///
/// Returns an error if the database path cannot be resolved, the parent
/// directory cannot be created, or `SQLite` cannot open the file.
pub fn open_storage(
    db_override: Option<&std::path::Path>,
    book: Option<&str>,
) -> Result<Storage, CliError> {
    let path = match db_override {
        Some(p) => p.to_path_buf(),
        None => default_db_path(book)?,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Storage::open(&path).map_err(Into::into)
}

/// # Errors
///
/// Returns an error if the user data directory cannot be determined, or if
/// the `book` name is empty or contains non-`is_ascii_alphanumeric`
/// characters.
pub fn default_db_path(book: Option<&str>) -> Result<PathBuf, CliError> {
    let dir = dirs::data_dir().ok_or_else(|| {
        CliError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not determine user data directory",
        ))
    })?;
    let filename = match book {
        Some(n) => {
            validate_book_name(n)?;
            format!("ledger.{n}.db")
        }
        None => "ledger.db".to_string(),
    };
    Ok(dir.join("ledger").join(filename))
}

fn validate_book_name(name: &str) -> Result<(), CliError> {
    if name.is_empty() {
        return Err(CliError::BookName("(empty)".to_string()));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(CliError::BookName(name.to_string()));
    }
    Ok(())
}
