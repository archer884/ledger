use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rust_decimal::Decimal;
use ulid::Ulid;

use crate::model::{
    Account, AccountClass, AccountId, AccountIdError, CloseInfo, Entry, ReconstructError,
    Transaction, TransactionError, TransactionId, TransactionKind, closes,
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

    /// Close the fiscal year: zero every income and expense account into equity/net
    Close(CloseArgs),

    /// Record opening balances on an empty database (plugs equity/net)
    Open(OpenArgs),

    /// Generate an audit report for a date range or fiscal year
    Audit(AuditArgs),

    /// Print the `ledger add` command that would recreate a transaction
    Reconstruct(ReconstructArgs),

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

    /// Role this transaction plays in the book. Emitted by `reconstruct` so
    /// a rebuilt ledger keeps its period boundaries; you rarely pass it by
    /// hand, since `close` and `open` set it for you.
    #[arg(long, value_enum, default_value_t = TransactionKindArg::Normal)]
    pub kind: TransactionKindArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TransactionKindArg {
    Normal,
    Close,
    Open,
}

impl From<TransactionKindArg> for TransactionKind {
    fn from(value: TransactionKindArg) -> Self {
        match value {
            TransactionKindArg::Normal => Self::Normal,
            TransactionKindArg::Close => Self::Close,
            TransactionKindArg::Open => Self::Open,
        }
    }
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

#[derive(Args)]
pub struct CloseArgs {
    /// Date of the closing transaction (YYYY-MM-DD), defaults to today
    #[arg(long)]
    pub date: Option<String>,

    /// Memo describing the close
    #[arg(long)]
    pub memo: Option<String>,
}

#[derive(Args)]
pub struct OpenArgs {
    /// Known opening balance in the form ID:CLASS:AMOUNT (e.g. checking:asset:1000)
    #[arg(long = "entry", value_name = "ID:CLASS:AMOUNT", required = true)]
    pub entries: Vec<String>,

    /// Date of the opening transaction (YYYY-MM-DD), defaults to today
    #[arg(long)]
    pub date: Option<String>,

    /// Memo describing the opening
    #[arg(long)]
    pub memo: Option<String>,
}

#[derive(Args)]
pub struct AuditArgs {
    /// Start date of the report (inclusive, YYYY-MM-DD)
    #[arg(long, required_unless_present = "fy")]
    pub from: Option<String>,

    /// End date of the report (exclusive, YYYY-MM-DD)
    #[arg(long, required_unless_present = "fy")]
    pub to: Option<String>,

    /// Fiscal year to report on: the calendar year of its closing
    /// transaction, or "latest" for the most recent close
    #[arg(long, conflicts_with_all = ["from", "to"])]
    pub fy: Option<String>,

    /// Emit the report as a JSON blob instead of human-readable text
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ReconstructArgs {
    /// Transaction ID (ULID) to reconstruct (required unless --all is set)
    #[arg(value_name = "TX_ID", required_unless_present = "all")]
    pub tx_id: Option<String>,

    /// Reconstruct every transaction in the database, in posted order
    #[arg(long, conflicts_with = "tx_id")]
    pub all: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum AccountClassArg {
    Asset,
    Liability,
    Equity,
    Income,
    Expense,
}

impl From<AccountClassArg> for AccountClass {
    fn from(arg: AccountClassArg) -> Self {
        match arg {
            AccountClassArg::Asset => AccountClass::Asset,
            AccountClassArg::Liability => AccountClass::Liability,
            AccountClassArg::Equity => AccountClass::Equity,
            AccountClassArg::Income => AccountClass::Income,
            AccountClassArg::Expense => AccountClass::Expense,
        }
    }
}

#[derive(Debug)]
pub enum CliError {
    Storage(StorageError),
    Transaction(TransactionError),
    AccountId(AccountIdError),
    Reconstruct(ReconstructError),
    Entry(String),
    Amount(String),
    Date(String),
    BookName(String),
    TxId(String),
    Close(String),
    Open(String),
    Fy(String),
    Tui(TuiError),
    Io(std::io::Error),
    Serde(serde_json::Error),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "{e}"),
            Self::Transaction(e) => write!(f, "{e}"),
            Self::AccountId(e) => write!(f, "{e}"),
            Self::Reconstruct(e) => write!(f, "{e}"),
            Self::Entry(s) => write!(f, "invalid entry: {s} (expected ID[:CLASS]:AMOUNT)"),
            Self::Amount(s) => write!(f, "invalid amount: {s}"),
            Self::Date(s) => write!(f, "invalid date: {s}"),
            Self::BookName(s) => write!(
                f,
                "invalid book name: {s:?} (must contain only letters and numbers)"
            ),
            Self::TxId(s) => write!(f, "invalid transaction id: {s}"),
            Self::Close(s) => write!(f, "close: {s}"),
            Self::Open(s) => write!(f, "open: {s}"),
            Self::Fy(s) => write!(f, "fiscal year: {s}"),
            Self::Tui(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Serde(e) => write!(f, "json: {e}"),
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

impl From<ReconstructError> for CliError {
    fn from(e: ReconstructError) -> Self {
        Self::Reconstruct(e)
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

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e)
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
        Command::Close(args) => run_close(&args, &storage),
        Command::Open(args) => run_open(&args, &storage),
        Command::Audit(args) => run_audit(&args, &storage),
        Command::Reconstruct(args) => run_reconstruct(&args, &storage),
        Command::Tui(_args) => crate::tui::run(storage).map_err(Into::into),
    }
}

fn run_add(args: &AddArgs, storage: &Storage) -> Result<(), CliError> {
    execute_transaction(
        args.date.as_deref(),
        args.memo.as_deref(),
        &args.entries,
        args.kind.into(),
        storage,
    )
}

fn run_income(args: &IncomeArgs, storage: &Storage) -> Result<(), CliError> {
    let amount = Decimal::from_str(&args.amount).map_err(|e| CliError::Amount(format!("{e}")))?;
    let entries = vec![
        format!("checking:asset:{amount}"),
        format!("income:income:-{amount}"),
    ];
    execute_transaction(
        args.date.as_deref(),
        args.memo.as_deref(),
        &entries,
        TransactionKind::Normal,
        storage,
    )
}

fn run_close(args: &CloseArgs, storage: &Storage) -> Result<(), CliError> {
    let classes = account_classes(storage)?;
    let balances = storage.balances()?;

    let mut nominals: Vec<(AccountId, &'static str, Decimal)> = balances
        .iter()
        .filter_map(|(id, balance)| match classes.get(id) {
            Some(AccountClass::Income) if *balance != Decimal::ZERO => {
                Some((id.clone(), "income", *balance))
            }
            Some(AccountClass::Expense) if *balance != Decimal::ZERO => {
                Some((id.clone(), "expense", *balance))
            }
            _ => None,
        })
        .collect();

    if nominals.is_empty() {
        return Err(CliError::Close(
            "nothing to close: every income and expense account is already zero".to_string(),
        ));
    }
    nominals.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));

    let nominal_sum: Decimal = nominals.iter().map(|(_, _, b)| *b).sum();
    let net_income = -nominal_sum;
    let plug = nominal_sum;

    let mut entries: Vec<String> = nominals
        .iter()
        .map(|(id, class, balance)| format!("{id}:{class}:{}", -balance))
        .collect();
    entries.push(format!("equity/net:equity:{plug}"));

    execute_transaction(
        args.date.as_deref(),
        args.memo.as_deref(),
        &entries,
        TransactionKind::Close,
        storage,
    )?;

    for (id, class, balance) in &nominals {
        println!("zeroed {id} ({class}) by {}", -balance);
    }
    println!("net income {net_income} rolled into equity/net");
    Ok(())
}

fn run_open(args: &OpenArgs, storage: &Storage) -> Result<(), CliError> {
    if !storage.list_transactions()?.is_empty() {
        return Err(CliError::Open(
            "the ledger already has transactions; `open` is only for an empty database".to_string(),
        ));
    }

    let mut sum = Decimal::ZERO;
    for entry_str in &args.entries {
        sum += parse_entry(entry_str)?.amount;
    }

    let plug = -sum;
    let mut entries = args.entries.clone();
    entries.push(format!("equity/net:equity:{plug}"));

    execute_transaction(
        args.date.as_deref(),
        args.memo.as_deref(),
        &entries,
        TransactionKind::Open,
        storage,
    )?;
    println!("recorded opening balances; equity/net plugged by {plug}");
    Ok(())
}

fn account_classes(storage: &Storage) -> Result<HashMap<AccountId, AccountClass>, StorageError> {
    Ok(storage
        .list_accounts()?
        .into_iter()
        .map(|a| (a.id, a.class))
        .collect())
}
fn run_reconstruct(args: &ReconstructArgs, storage: &Storage) -> Result<(), CliError> {
    let classes: HashMap<AccountId, AccountClass> = storage
        .list_accounts()?
        .into_iter()
        .map(|a| (a.id, a.class))
        .collect();
    if args.all {
        let transactions = storage.list_transactions()?;
        let out = crate::model::render_all_add_commands(&transactions, &classes)?;
        print!("{out}");
        return Ok(());
    }
    let tx_id_str = args
        .tx_id
        .as_deref()
        .ok_or_else(|| CliError::TxId("TX_ID is required (or pass --all)".to_string()))?;
    let ulid = Ulid::from_str(tx_id_str).map_err(|e| CliError::TxId(format!("{e}")))?;
    let tx = storage.get_transaction(TransactionId(ulid))?;
    let cmd = tx.render_add_command(&classes)?;
    print!("{cmd}");
    Ok(())
}

fn execute_transaction(
    date: Option<&str>,
    memo: Option<&str>,
    entry_strs: &[String],
    kind: TransactionKind,
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
    let tx = if kind == TransactionKind::Normal {
        tx
    } else {
        tx.with_kind(kind, &account_classes(storage)?)?
    };

    storage.save_transaction(&tx)?;

    println!("added transaction {}", tx.id.0);
    Ok(())
}

/// Build the set of "our" accounts for an audit: every account of class
/// `asset`. Receipts flow into these, disbursements flow out of them.
fn asset_accounts(storage: &Storage) -> Result<HashSet<AccountId>, CliError> {
    let accounts = storage.list_accounts()?;
    Ok(accounts
        .into_iter()
        .filter(|a| a.class == AccountClass::Asset)
        .map(|a| a.id)
        .collect())
}

#[derive(Debug)]
struct AuditReport {
    from: Timestamp,
    to: Timestamp,
    beginning_balances: Vec<(AccountId, Decimal)>,
    ending_balances: Vec<(AccountId, Decimal)>,
    receipts: Vec<(AccountId, Decimal)>,
    disbursements: Vec<(AccountId, Decimal)>,
    total_receipts: Decimal,
    total_disbursements: Decimal,
}

fn run_audit(args: &AuditArgs, storage: &Storage) -> Result<(), CliError> {
    let transactions = storage.list_transactions()?;
    let (from, to) = match &args.fy {
        Some(fy) => {
            let closes = closes(&transactions);
            resolve_fy_period(&transactions, &closes, fy)?
        }
        None => (
            parse_date(args.from.as_deref().unwrap_or(""))?,
            parse_date(args.to.as_deref().unwrap_or(""))?,
        ),
    };
    if to < from {
        return Err(CliError::Date(format!(
            "report end ({}) is before start ({})",
            format_date(to),
            format_date(from)
        )));
    }
    let our_accounts = asset_accounts(storage)?;
    let report = build_audit_report(&transactions, from, to, &our_accounts);

    if args.json {
        print_audit_json(&report)?;
    } else {
        print_audit_text(&report);
    }
    Ok(())
}

/// Resolve an `--fy` argument to a `(from, to)` audit period. `latest`
/// selects the most recent close; anything else must be the calendar
/// year (UTC) of a close's posted date. The period starts at the
/// previous close (or at the first transaction, for the first fiscal
/// year) and ends the day after the selected close, so the closing
/// transaction itself is included.
///
/// # Errors
///
/// Returns `CliError::Fy` if the ledger has no closes, the argument is
/// neither `latest` nor a parseable year, no close falls in the given
/// year, or more than one does.
fn resolve_fy_period(
    transactions: &[Transaction],
    closes: &[CloseInfo],
    fy: &str,
) -> Result<(Timestamp, Timestamp), CliError> {
    if closes.is_empty() {
        return Err(CliError::Fy(
            "no closing transactions in the ledger; run `ledger close` first or use --from/--to"
                .to_string(),
        ));
    }

    let close_year = |c: &CloseInfo| c.posted_at.to_zoned(TimeZone::UTC).year();
    let index = if fy == "latest" {
        closes.len() - 1
    } else {
        let year: i16 = fy.parse().map_err(|_| {
            CliError::Fy(format!(
                "invalid year {fy:?} (expected a year like 2025, or \"latest\")"
            ))
        })?;
        let matching: Vec<usize> = closes
            .iter()
            .enumerate()
            .filter(|(_, c)| close_year(c) == year)
            .map(|(i, _)| i)
            .collect();
        match matching.as_slice() {
            [] => {
                let mut years: Vec<i16> = closes.iter().map(close_year).collect();
                years.sort_unstable();
                years.dedup();
                let list = years
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(CliError::Fy(format!(
                    "no close in {year}; closed years: {list}"
                )));
            }
            [only] => *only,
            _ => {
                return Err(CliError::Fy(
                    "multiple closes in {year}; use --from/--to to pick an exact range".to_string(),
                ));
            }
        }
    };

    let close = closes[index];
    let from = if index == 0 {
        transactions
            .first()
            .map_or(close.posted_at, |t| t.posted_at)
    } else {
        closes[index - 1].posted_at
    };
    let to = close
        .posted_at
        .to_zoned(TimeZone::UTC)
        .date()
        .tomorrow()
        .map_err(|e| CliError::Fy(format!("day after close: {e}")))?
        .at(0, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .map_err(|e| CliError::Fy(format!("day after close: {e}")))?
        .timestamp();
    Ok((from, to))
}

/// Build an audit report from a set of transactions. Pure and testable: the
/// storage layer is not involved.
///
/// `from` is inclusive and `to` is exclusive. "Our" accounts are the asset
/// accounts in `our_accounts`. The beginning and ending balances are broken
/// down per asset account, computed as the sum of each account's entries over
/// all transactions strictly before `from` (beginning) and strictly before
/// `to` (ending). Within the period, a transaction whose net effect on our
/// asset accounts is positive is a receipt (attributed to the non-asset
/// accounts it leaves), and one whose net effect is negative is a
/// disbursement (attributed to the non-asset accounts it enters). The totals
/// are the net change of our asset accounts, so the sum of beginning balances
/// plus total receipts minus total disbursements ties to the sum of ending
/// balances. Transfers between our own asset accounts net to zero and are
/// neither receipts nor disbursements.
fn build_audit_report(
    transactions: &[Transaction],
    from: Timestamp,
    to: Timestamp,
    our_accounts: &HashSet<AccountId>,
) -> AuditReport {
    let mut asset_ids: Vec<AccountId> = our_accounts.iter().cloned().collect();
    asset_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    let beginning_balances = balances_until(transactions, from, &asset_ids);
    let ending_balances = balances_until(transactions, to, &asset_ids);

    let mut receipts: HashMap<AccountId, Decimal> = HashMap::new();
    let mut disbursements: HashMap<AccountId, Decimal> = HashMap::new();
    let mut total_receipts = Decimal::ZERO;
    let mut total_disbursements = Decimal::ZERO;

    for tx in transactions
        .iter()
        .filter(|tx| tx.posted_at >= from && tx.posted_at < to)
    {
        let asset_delta: Decimal = tx
            .entries
            .iter()
            .filter(|e| our_accounts.contains(&e.account))
            .map(|e| e.amount)
            .sum();

        if asset_delta > Decimal::ZERO {
            total_receipts += asset_delta;
            for e in &tx.entries {
                if !our_accounts.contains(&e.account) && e.amount < Decimal::ZERO {
                    *receipts.entry(e.account.clone()).or_default() += -e.amount;
                }
            }
        } else if asset_delta < Decimal::ZERO {
            total_disbursements += -asset_delta;
            for e in &tx.entries {
                if !our_accounts.contains(&e.account) && e.amount > Decimal::ZERO {
                    *disbursements.entry(e.account.clone()).or_default() += e.amount;
                }
            }
        }
    }

    let receipts = sort_by_account(receipts);
    let disbursements = sort_by_account(disbursements);

    AuditReport {
        from,
        to,
        beginning_balances,
        ending_balances,
        receipts,
        disbursements,
        total_receipts,
        total_disbursements,
    }
}

/// Sum each account in `asset_ids` over all entries of transactions strictly
/// before `boundary`. Returns one `(AccountId, Decimal)` per input account,
/// in the same order as `asset_ids`, including zero balances.
fn balances_until(
    transactions: &[Transaction],
    boundary: Timestamp,
    asset_ids: &[AccountId],
) -> Vec<(AccountId, Decimal)> {
    let mut sums: Vec<Decimal> = vec![Decimal::ZERO; asset_ids.len()];
    let index: HashMap<&AccountId, usize> =
        asset_ids.iter().enumerate().map(|(i, a)| (a, i)).collect();

    for tx in transactions.iter().filter(|tx| tx.posted_at < boundary) {
        for e in &tx.entries {
            if let Some(&i) = index.get(&e.account) {
                sums[i] += e.amount;
            }
        }
    }

    asset_ids
        .iter()
        .zip(sums)
        .map(|(a, s)| (a.clone(), s))
        .collect()
}

fn sort_by_account(map: HashMap<AccountId, Decimal>) -> Vec<(AccountId, Decimal)> {
    let mut v: Vec<(AccountId, Decimal)> = map.into_iter().collect();
    v.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    v
}

fn format_date(t: Timestamp) -> String {
    t.to_zoned(TimeZone::UTC).date().to_string()
}

fn format_money(d: &Decimal) -> String {
    format!("{d:.2}")
}

fn print_audit_text(report: &AuditReport) {
    println!(
        "Audit Report ({} to {})",
        format_date(report.from),
        format_date(report.to)
    );
    println!("{}", "=".repeat(50));

    print_balance_section("Beginning balances by account:", &report.beginning_balances);
    println!();
    print_balance_section("Ending balances by account:", &report.ending_balances);
    println!();

    println!("Receipts by account:");
    if report.receipts.is_empty() {
        println!("  (none)");
    } else {
        for (acct, amt) in &report.receipts {
            println!("  {:<28}{:>14}", acct.as_str(), format_money(amt));
        }
    }
    println!(
        "  {:<28}{:>14}",
        "Total receipts",
        format_money(&report.total_receipts)
    );
    println!();
    println!("Disbursements by account:");
    if report.disbursements.is_empty() {
        println!("  (none)");
    } else {
        for (acct, amt) in &report.disbursements {
            println!("  {:<28}{:>14}", acct.as_str(), format_money(amt));
        }
    }
    println!(
        "  {:<28}{:>14}",
        "Total disbursements",
        format_money(&report.total_disbursements)
    );
}

fn print_balance_section(heading: &str, balances: &[(AccountId, Decimal)]) {
    println!("{heading}");
    let total: Decimal = balances.iter().map(|(_, a)| *a).sum();
    if balances.is_empty() {
        println!("  (none)");
    } else {
        for (acct, amt) in balances {
            println!("  {:<28}{:>14}", acct.as_str(), format_money(amt));
        }
    }
    println!("  {:<28}{:>14}", "Total", format_money(&total));
}

#[derive(serde::Serialize)]
struct AuditJson {
    from: String,
    to: String,
    beginning_balances: BTreeMap<String, String>,
    ending_balances: BTreeMap<String, String>,
    receipts: BTreeMap<String, String>,
    disbursements: BTreeMap<String, String>,
    total_receipts: String,
    total_disbursements: String,
}

fn print_audit_json(report: &AuditReport) -> Result<(), CliError> {
    let beginning_balances = balances_to_map(&report.beginning_balances);
    let ending_balances = balances_to_map(&report.ending_balances);
    let receipts = balances_to_map(&report.receipts);
    let disbursements = balances_to_map(&report.disbursements);
    let json = AuditJson {
        from: format_date(report.from),
        to: format_date(report.to),
        beginning_balances,
        ending_balances,
        receipts,
        disbursements,
        total_receipts: format_money(&report.total_receipts),
        total_disbursements: format_money(&report.total_disbursements),
    };
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

fn balances_to_map(balances: &[(AccountId, Decimal)]) -> BTreeMap<String, String> {
    balances
        .iter()
        .map(|(acct, amt)| (acct.to_string(), format_money(amt)))
        .collect()
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
                "income" => AccountClassArg::Income,
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

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::Date;
    use jiff::tz::TimeZone;

    fn date(s: &str) -> Timestamp {
        Date::strptime("%Y-%m-%d", s)
            .expect("date parses")
            .at(0, 0, 0, 0)
            .to_zoned(TimeZone::UTC)
            .expect("midnight UTC is never ambiguous")
            .timestamp()
    }

    fn entry(account: &str, amount: i64) -> Entry {
        Entry {
            account: AccountId::parse(account).expect("account parses"),
            amount: Decimal::from(amount),
        }
    }

    fn tx(day: &str, entries: Vec<Entry>) -> Transaction {
        Transaction::new(TransactionId::new(), date(day), String::new(), entries)
            .expect("transaction balances")
    }

    fn assets(names: &[&str]) -> HashSet<AccountId> {
        names
            .iter()
            .map(|n| AccountId::parse(n).expect("account parses"))
            .collect()
    }

    fn total(balances: &[(AccountId, Decimal)]) -> Decimal {
        balances.iter().map(|(_, a)| *a).sum()
    }

    fn acct(id: &str) -> AccountId {
        AccountId::parse(id).unwrap()
    }

    #[test]
    fn parse_entry_accepts_income_class() {
        let parsed = parse_entry("salary:income:1000").expect("parses");
        assert_eq!(parsed.id.as_str(), "salary");
        assert_eq!(parsed.amount, Decimal::from(1000));
        assert!(matches!(parsed.class, Some(AccountClassArg::Income)));
    }

    #[test]
    fn audit_report_reconciles() {
        let our = assets(&["checking"]);
        let transactions = vec![
            tx(
                "2024-01-15",
                vec![entry("checking", 500), entry("income", -500)],
            ),
            tx(
                "2024-02-10",
                vec![entry("checking", 1000), entry("income", -1000)],
            ),
            tx(
                "2024-02-20",
                vec![entry("checking", -300), entry("rent", 300)],
            ),
            tx(
                "2024-03-05",
                vec![entry("checking", -200), entry("rent", 200)],
            ),
        ];
        let report =
            build_audit_report(&transactions, date("2024-02-01"), date("2024-03-01"), &our);

        assert_eq!(total(&report.beginning_balances), Decimal::from(500));
        assert_eq!(report.total_receipts, Decimal::from(1000));
        assert_eq!(report.total_disbursements, Decimal::from(300));
        assert_eq!(report.receipts, vec![(acct("income"), Decimal::from(1000))]);
        assert_eq!(
            report.disbursements,
            vec![(acct("rent"), Decimal::from(300))]
        );
        let ending =
            total(&report.beginning_balances) + report.total_receipts - report.total_disbursements;
        assert_eq!(ending, Decimal::from(1200));
        assert_eq!(total(&report.ending_balances), ending);
    }

    #[test]
    fn audit_report_empty_period() {
        let our = assets(&["checking"]);
        let transactions = vec![tx(
            "2024-01-15",
            vec![entry("checking", 500), entry("income", -500)],
        )];
        let report =
            build_audit_report(&transactions, date("2024-02-01"), date("2024-03-01"), &our);

        assert_eq!(total(&report.beginning_balances), Decimal::from(500));
        assert_eq!(report.total_receipts, Decimal::ZERO);
        assert_eq!(report.total_disbursements, Decimal::ZERO);
        assert!(report.receipts.is_empty());
        assert!(report.disbursements.is_empty());
    }

    #[test]
    fn audit_report_ignores_pure_non_asset_transactions() {
        let our = assets(&["checking", "savings"]);
        let transactions = vec![tx(
            "2024-02-10",
            vec![entry("expense", 100), entry("income", -100)],
        )];
        let report =
            build_audit_report(&transactions, date("2024-02-01"), date("2024-03-01"), &our);

        assert_eq!(total(&report.beginning_balances), Decimal::ZERO);
        assert_eq!(report.total_receipts, Decimal::ZERO);
        assert_eq!(report.total_disbursements, Decimal::ZERO);
    }

    #[test]
    fn audit_report_from_is_inclusive_to_is_exclusive() {
        let our = assets(&["checking"]);
        let transactions = vec![
            tx(
                "2024-02-01",
                vec![entry("checking", 100), entry("income", -100)],
            ),
            tx(
                "2024-02-28",
                vec![entry("checking", 200), entry("income", -200)],
            ),
            tx(
                "2024-03-01",
                vec![entry("checking", 400), entry("income", -400)],
            ),
        ];
        let report =
            build_audit_report(&transactions, date("2024-02-01"), date("2024-03-01"), &our);

        assert_eq!(total(&report.beginning_balances), Decimal::ZERO);
        assert_eq!(report.total_receipts, Decimal::from(300));
        assert_eq!(report.total_disbursements, Decimal::ZERO);
    }

    #[test]
    fn audit_report_sums_all_asset_accounts() {
        let our = assets(&["checking", "savings"]);
        let transactions = vec![
            tx(
                "2024-01-15",
                vec![entry("checking", 500), entry("income", -500)],
            ),
            tx(
                "2024-01-20",
                vec![entry("savings", 300), entry("income", -300)],
            ),
            tx(
                "2024-02-10",
                vec![entry("savings", 1000), entry("income", -1000)],
            ),
            tx(
                "2024-02-20",
                vec![entry("checking", -250), entry("rent", 250)],
            ),
        ];
        let report =
            build_audit_report(&transactions, date("2024-02-01"), date("2024-03-01"), &our);

        assert_eq!(total(&report.beginning_balances), Decimal::from(800));
        assert_eq!(report.total_receipts, Decimal::from(1000));
        assert_eq!(report.total_disbursements, Decimal::from(250));
        let ending =
            total(&report.beginning_balances) + report.total_receipts - report.total_disbursements;
        assert_eq!(ending, Decimal::from(1550));
        assert_eq!(total(&report.ending_balances), ending);
    }

    #[test]
    fn audit_report_breaks_down_beginning_and_ending_by_account() {
        let our = assets(&["checking", "savings"]);
        let transactions = vec![
            tx(
                "2024-01-15",
                vec![entry("checking", 500), entry("income", -500)],
            ),
            tx(
                "2024-01-20",
                vec![entry("savings", 300), entry("income", -300)],
            ),
            tx(
                "2024-02-10",
                vec![entry("savings", 1000), entry("income", -1000)],
            ),
            tx(
                "2024-02-20",
                vec![entry("checking", -250), entry("rent", 250)],
            ),
        ];
        let report =
            build_audit_report(&transactions, date("2024-02-01"), date("2024-03-01"), &our);

        assert_eq!(
            report.beginning_balances,
            vec![
                (acct("checking"), Decimal::from(500)),
                (acct("savings"), Decimal::from(300))
            ]
        );
        assert_eq!(
            report.ending_balances,
            vec![
                (acct("checking"), Decimal::from(250)),
                (acct("savings"), Decimal::from(1300))
            ]
        );
    }

    #[test]
    fn audit_report_transfers_between_asset_accounts_are_neutral() {
        let our = assets(&["checking", "savings"]);
        let transactions = vec![tx(
            "2024-02-10",
            vec![entry("savings", 1000), entry("checking", -1000)],
        )];
        let report =
            build_audit_report(&transactions, date("2024-02-01"), date("2024-03-01"), &our);

        assert_eq!(total(&report.beginning_balances), Decimal::ZERO);
        assert_eq!(report.total_receipts, Decimal::ZERO);
        assert_eq!(report.total_disbursements, Decimal::ZERO);
        assert!(report.receipts.is_empty());
        assert!(report.disbursements.is_empty());
    }

    fn close_info(day: &str) -> CloseInfo {
        CloseInfo {
            id: TransactionId::new(),
            posted_at: date(day),
        }
    }

    #[test]
    fn fy_latest_resolves_to_the_last_close() {
        let transactions = vec![
            tx(
                "2024-01-05",
                vec![entry("checking", 1), entry("income", -1)],
            ),
            tx(
                "2025-01-05",
                vec![entry("checking", 2), entry("income", -2)],
            ),
        ];
        let closes = vec![close_info("2024-12-31"), close_info("2025-12-31")];
        let (from, to) = resolve_fy_period(&transactions, &closes, "latest").expect("resolves");
        assert_eq!(from, date("2024-12-31"));
        assert_eq!(to, date("2026-01-01"));
    }

    #[test]
    fn fy_first_year_starts_at_first_transaction() {
        let transactions = vec![tx(
            "2025-01-01",
            vec![entry("checking", 100), entry("equity/net", -100)],
        )];
        let closes = vec![close_info("2025-12-31")];
        let (from, to) = resolve_fy_period(&transactions, &closes, "2025").expect("resolves");
        assert_eq!(from, date("2025-01-01"));
        assert_eq!(to, date("2026-01-01"));
    }

    #[test]
    fn fy_unknown_year_lists_closed_years() {
        let transactions = vec![tx(
            "2024-01-05",
            vec![entry("checking", 1), entry("income", -1)],
        )];
        let closes = vec![close_info("2024-12-31")];
        let err = resolve_fy_period(&transactions, &closes, "2027").expect_err("no close");
        assert!(matches!(err, CliError::Fy(_)));
        assert!(err.to_string().contains("closed years: 2024"), "{err}");
    }

    #[test]
    fn fy_ambiguous_year_errors() {
        let transactions = vec![tx(
            "2025-01-05",
            vec![entry("checking", 1), entry("income", -1)],
        )];
        let closes = vec![close_info("2025-06-30"), close_info("2025-12-31")];
        let err = resolve_fy_period(&transactions, &closes, "2025").expect_err("ambiguous");
        assert!(matches!(err, CliError::Fy(_)));
    }

    #[test]
    fn fy_without_closes_errors() {
        let err = resolve_fy_period(&[], &[], "latest").expect_err("no closes");
        assert!(matches!(err, CliError::Fy(_)));
    }

    #[test]
    fn fy_rejects_non_year_argument() {
        let transactions = vec![tx(
            "2024-01-05",
            vec![entry("checking", 1), entry("income", -1)],
        )];
        let closes = vec![close_info("2024-12-31")];
        let err = resolve_fy_period(&transactions, &closes, "banana").expect_err("not a year");
        assert!(matches!(err, CliError::Fy(_)));
    }

    #[test]
    fn close_zeroes_nominals_and_plugs_equity_net() {
        let storage = Storage::in_memory().expect("in-memory db");
        for (id, class) in [
            ("checking", AccountClass::Asset),
            ("salary", AccountClass::Income),
            ("expenses/food", AccountClass::Expense),
        ] {
            storage
                .register_account(&Account {
                    id: AccountId::parse(id).expect("account parses"),
                    class,
                })
                .expect("registered");
        }
        storage
            .save_transaction(&tx(
                "2025-06-01",
                vec![entry("checking", 2000), entry("salary", -2000)],
            ))
            .expect("saved");
        storage
            .save_transaction(&tx(
                "2025-06-02",
                vec![entry("expenses/food", 500), entry("checking", -500)],
            ))
            .expect("saved");

        let args = CloseArgs {
            date: Some("2025-12-31".to_string()),
            memo: Some("FY2025".to_string()),
        };
        run_close(&args, &storage).expect("close works");

        let balances = storage.balances().expect("balances");
        assert_eq!(balances[&acct("salary")], Decimal::ZERO);
        assert_eq!(balances[&acct("expenses/food")], Decimal::ZERO);
        assert_eq!(balances[&acct("checking")], Decimal::from(1500));
        assert_eq!(balances[&acct("equity/net")], Decimal::from(-1500));

        let transactions = storage.list_transactions().expect("list");
        let found = closes(&transactions);
        assert_eq!(found.len(), 1, "close should be marked, not inferred");
        let closing = transactions
            .iter()
            .find(|t| t.id == found[0].id)
            .expect("close is in the list");
        assert_eq!(closing.kind, TransactionKind::Close);
        assert_eq!(closing.memo, "FY2025");
    }

    /// `open` marks its transaction too, so "since open" is exact rather
    /// than a guess at whichever transaction happens to sort first.
    #[test]
    fn open_marks_its_transaction_as_a_boundary() {
        let storage = Storage::in_memory().expect("in-memory db");
        let args = OpenArgs {
            date: Some("2025-01-01".to_string()),
            memo: None,
            entries: vec!["checking:asset:1000".to_string()],
        };
        run_open(&args, &storage).expect("open works");

        let transactions = storage.list_transactions().expect("list");
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].kind, TransactionKind::Open);
        assert!(
            closes(&transactions).is_empty(),
            "an open is not a close, even though both start a period"
        );
        assert_eq!(
            crate::model::period_start(&transactions),
            Some(transactions[0].posted_at)
        );
    }

    /// The round trip that makes recorded kinds safe: `reconstruct --all`
    /// emits `--kind`, and replaying it restores the boundary.
    #[test]
    fn reconstruct_round_trips_a_close_through_add() {
        let storage = Storage::in_memory().expect("in-memory db");
        for (id, class) in [
            ("checking", AccountClass::Asset),
            ("salary", AccountClass::Income),
            ("equity/net", AccountClass::Equity),
        ] {
            storage
                .register_account(&Account {
                    id: AccountId::parse(id).expect("account parses"),
                    class,
                })
                .expect("registered");
        }
        storage
            .save_transaction(&tx(
                "2025-06-01",
                vec![entry("checking", 2000), entry("salary", -2000)],
            ))
            .expect("saved");
        run_close(
            &CloseArgs {
                date: Some("2025-12-31".to_string()),
                memo: None,
            },
            &storage,
        )
        .expect("close works");

        let original = storage.list_transactions().expect("list");
        let classes = account_classes(&storage).expect("classes");
        let script = crate::model::render_all_add_commands(&original, &classes).expect("renders");
        assert!(script.contains("--kind close"), "{script}");

        // Replay the emitted commands into a fresh book.
        let rebuilt = Storage::in_memory().expect("in-memory db");
        for (id, class) in [
            ("checking", AccountClass::Asset),
            ("salary", AccountClass::Income),
            ("equity/net", AccountClass::Equity),
        ] {
            rebuilt
                .register_account(&Account {
                    id: AccountId::parse(id).expect("account parses"),
                    class,
                })
                .expect("registered");
        }
        for source in &original {
            let kind = match source.kind {
                TransactionKind::Normal => TransactionKindArg::Normal,
                TransactionKind::Close => TransactionKindArg::Close,
                TransactionKind::Open => TransactionKindArg::Open,
            };
            let entries: Vec<String> = source
                .entries
                .iter()
                .map(|e| format!("{}:{}", e.account, e.amount))
                .collect();
            run_add(
                &AddArgs {
                    date: Some(source.posted_at.to_zoned(TimeZone::UTC).date().to_string()),
                    memo: None,
                    entries,
                    kind,
                },
                &rebuilt,
            )
            .expect("replayed");
        }

        let replayed = rebuilt.list_transactions().expect("list");
        assert_eq!(replayed.len(), original.len());
        assert_eq!(
            closes(&replayed).len(),
            1,
            "the close must survive the rebuild"
        );
        assert_eq!(
            rebuilt.balances().expect("balances"),
            storage.balances().expect("balances")
        );
    }

    #[test]
    fn close_with_nothing_to_close_errors() {
        let storage = Storage::in_memory().expect("in-memory db");
        storage
            .register_account(&Account {
                id: AccountId::parse("salary").expect("account parses"),
                class: AccountClass::Income,
            })
            .expect("registered");
        let args = CloseArgs {
            date: None,
            memo: None,
        };
        let err = run_close(&args, &storage).expect_err("nothing to close");
        assert!(matches!(err, CliError::Close(_)));
    }

    #[test]
    fn open_plugs_the_difference_into_equity_net() {
        let storage = Storage::in_memory().expect("in-memory db");
        let args = OpenArgs {
            entries: vec![
                "checking:asset:1000".to_string(),
                "mortgage:liability:-300".to_string(),
            ],
            date: Some("2025-01-01".to_string()),
            memo: None,
        };
        run_open(&args, &storage).expect("open works");

        let balances = storage.balances().expect("balances");
        assert_eq!(balances[&acct("checking")], Decimal::from(1000));
        assert_eq!(balances[&acct("mortgage")], Decimal::from(-300));
        assert_eq!(balances[&acct("equity/net")], Decimal::from(-700));
        assert_eq!(storage.list_transactions().expect("list").len(), 1);
    }

    #[test]
    fn open_refuses_nonempty_ledger() {
        let storage = Storage::in_memory().expect("in-memory db");
        storage
            .register_account(&Account {
                id: AccountId::parse("checking").expect("account parses"),
                class: AccountClass::Asset,
            })
            .expect("registered");
        storage
            .register_account(&Account {
                id: AccountId::parse("income").expect("account parses"),
                class: AccountClass::Income,
            })
            .expect("registered");
        storage
            .save_transaction(&tx(
                "2025-06-01",
                vec![entry("checking", 100), entry("income", -100)],
            ))
            .expect("saved");

        let args = OpenArgs {
            entries: vec!["checking:asset:1000".to_string()],
            date: None,
            memo: None,
        };
        let err = run_open(&args, &storage).expect_err("non-empty ledger");
        assert!(matches!(err, CliError::Open(_)));
    }
}
