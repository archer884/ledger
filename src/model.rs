use std::collections::HashMap;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use rust_decimal::Decimal;
use ulid::Ulid;

const MAX_ACCOUNT_ID_LEN: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccountId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountIdError {
    Empty,
    TooLong { max: usize },
    InvalidChar(char),
}

impl std::fmt::Display for AccountIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("account id is empty"),
            Self::TooLong { max } => write!(f, "account id exceeds {max} characters"),
            Self::InvalidChar(c) => write!(f, "account id contains invalid character: {c:?}"),
        }
    }
}

impl std::error::Error for AccountIdError {}

impl AccountId {
    /// # Errors
    ///
    /// Returns `AccountIdError::Empty` if the input trims to nothing,
    /// `AccountIdError::TooLong` if the result exceeds `MAX_ACCOUNT_ID_LEN`,
    /// or `AccountIdError::InvalidChar` if the input contains a control
    /// character or one of the reserved separator characters.
    pub fn parse(input: &str) -> Result<Self, AccountIdError> {
        Ok(Self(normalize_account_id(input)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn normalize_account_id(input: &str) -> Result<String, AccountIdError> {
    let mut out = String::with_capacity(input.len());
    let mut content_started = false;
    let mut last_was_space = false;

    for ch in input.trim().chars() {
        if ch.is_whitespace() {
            if content_started && !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else if ch.is_control() || matches!(ch, '\\' | '.') {
            return Err(AccountIdError::InvalidChar(ch));
        } else {
            out.extend(ch.to_lowercase());
            content_started = true;
            last_was_space = false;
        }
    }

    while out.ends_with(' ') {
        out.pop();
    }

    if out.is_empty() {
        return Err(AccountIdError::Empty);
    }

    if out.len() > MAX_ACCOUNT_ID_LEN {
        return Err(AccountIdError::TooLong {
            max: MAX_ACCOUNT_ID_LEN,
        });
    }

    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransactionId(pub Ulid);

impl TransactionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::generate())
    }
}

impl Default for TransactionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountClass {
    Asset,
    Liability,
    Equity,
    Income,
    Expense,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: AccountId,
    pub class: AccountClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub account: AccountId,
    pub amount: Decimal,
}

/// What role a transaction plays in the book. `Normal` is the overwhelming
/// majority; the other two are period boundaries, posted by `ledger close`
/// and `ledger open`.
///
/// This is recorded rather than inferred. Working it out from the entry
/// structure after the fact is possible but fragile — it depends on reading
/// transactions in posted order, which same-day ties cannot guarantee.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TransactionKind {
    #[default]
    Normal,
    Close,
    Open,
}

impl TransactionKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Close => "close",
            Self::Open => "open",
        }
    }

    /// Both ends of a fiscal period. A summary treats them alike: each one
    /// starts a fresh period, and `Open` additionally means nothing precedes
    /// it.
    #[must_use]
    pub fn is_boundary(self) -> bool {
        matches!(self, Self::Close | Self::Open)
    }

    /// # Errors
    ///
    /// Returns `TransactionError::UnknownKind` if `s` is not a known kind.
    pub fn parse(s: &str) -> Result<Self, TransactionError> {
        match s {
            "normal" => Ok(Self::Normal),
            "close" => Ok(Self::Close),
            "open" => Ok(Self::Open),
            other => Err(TransactionError::UnknownKind(other.to_string())),
        }
    }
}

impl std::fmt::Display for TransactionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub id: TransactionId,
    pub posted_at: Timestamp,
    pub memo: String,
    pub entries: Vec<Entry>,
    pub kind: TransactionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionError {
    TooFewEntries {
        got: usize,
        min: usize,
    },
    Unbalanced {
        sum: Decimal,
    },
    UnknownKind(String),
    UnknownAccount(AccountId),
    WrongShape {
        kind: TransactionKind,
        account: AccountId,
        class: AccountClass,
    },
}

impl std::fmt::Display for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewEntries { got, min } => {
                write!(f, "transaction has {got} entries, minimum is {min}")
            }
            Self::Unbalanced { sum } => {
                write!(f, "entries do not sum to zero; sum is {sum}")
            }
            Self::UnknownKind(s) => {
                write!(
                    f,
                    "unknown transaction kind {s:?} (expected normal, close, or open)"
                )
            }
            Self::UnknownAccount(id) => write!(f, "unknown account: {id}"),
            Self::WrongShape {
                kind,
                account,
                class,
            } => {
                let (subject, rule) = match kind {
                    TransactionKind::Close => (
                        "a close",
                        "a close moves income and expense balances into equity",
                    ),
                    TransactionKind::Open => (
                        "an open",
                        "an open records asset and liability balances against equity",
                    ),
                    TransactionKind::Normal => (
                        "a normal transaction",
                        "normal transactions have no restriction",
                    ),
                };
                write!(
                    f,
                    "{subject} cannot touch {account} ({}); {rule}",
                    class_to_str(*class),
                )
            }
        }
    }
}

impl std::error::Error for TransactionError {}

impl Transaction {
    /// # Errors
    ///
    /// Returns `TransactionError::TooFewEntries` if `entries` has fewer than
    /// two elements, or `TransactionError::Unbalanced` if the entries do not
    /// sum to zero.
    pub fn new(
        id: TransactionId,
        posted_at: Timestamp,
        memo: String,
        entries: Vec<Entry>,
    ) -> Result<Self, TransactionError> {
        if entries.len() < 2 {
            return Err(TransactionError::TooFewEntries {
                got: entries.len(),
                min: 2,
            });
        }
        let sum: Decimal = entries.iter().map(|e| e.amount).sum();
        if sum != Decimal::ZERO {
            return Err(TransactionError::Unbalanced { sum });
        }
        Ok(Self {
            id,
            posted_at,
            memo,
            entries,
            kind: TransactionKind::Normal,
        })
    }

    /// Record this transaction as a period boundary.
    ///
    /// The kind is checked against the entries so a stored marker can never
    /// describe a transaction that isn't shaped like one: a close only moves
    /// nominal balances into equity, an open only records real balances
    /// against equity.
    ///
    /// # Errors
    ///
    /// Returns `TransactionError::UnknownAccount` if an entry names an
    /// account missing from `classes`, or `TransactionError::WrongShape` if
    /// an entry's class is not allowed for `kind`.
    pub fn with_kind(
        mut self,
        kind: TransactionKind,
        classes: &HashMap<AccountId, AccountClass>,
    ) -> Result<Self, TransactionError> {
        for entry in &self.entries {
            let class = *classes
                .get(&entry.account)
                .ok_or_else(|| TransactionError::UnknownAccount(entry.account.clone()))?;
            let allowed = match kind {
                TransactionKind::Normal => true,
                TransactionKind::Close => matches!(
                    class,
                    AccountClass::Income | AccountClass::Expense | AccountClass::Equity
                ),
                TransactionKind::Open => matches!(
                    class,
                    AccountClass::Asset | AccountClass::Liability | AccountClass::Equity
                ),
            };
            if !allowed {
                return Err(TransactionError::WrongShape {
                    kind,
                    account: entry.account.clone(),
                    class,
                });
            }
        }
        self.kind = kind;
        Ok(self)
    }

    pub(crate) fn from_raw(
        id: TransactionId,
        posted_at: Timestamp,
        memo: String,
        entries: Vec<Entry>,
        kind: TransactionKind,
    ) -> Self {
        Self {
            id,
            posted_at,
            memo,
            entries,
            kind,
        }
    }

    #[must_use]
    pub fn balance_by_account(&self) -> HashMap<AccountId, Decimal> {
        let mut balances = HashMap::new();
        for entry in &self.entries {
            *balances
                .entry(entry.account.clone())
                .or_insert(Decimal::ZERO) += entry.amount;
        }
        balances
    }

    /// Render this transaction as a `ledger add` command, multi-line with
    /// `\` continuations so the output can be pasted into a shell. Memo is
    /// omitted when empty. Account classes are looked up in `classes`;
    /// an entry whose account is missing from the map yields an
    /// `UnknownAccount` error.
    ///
    /// # Errors
    ///
    /// Returns `ReconstructError::UnknownAccount` if any entry's account is
    /// not present in `classes`.
    pub fn render_add_command(
        &self,
        classes: &HashMap<AccountId, AccountClass>,
    ) -> Result<String, ReconstructError> {
        let date = self.posted_at.to_zoned(TimeZone::UTC).date().to_string();
        let mut parts: Vec<String> = Vec::with_capacity(3 + self.entries.len());
        parts.push(format!("ledger add --date {date}"));
        if self.kind != TransactionKind::Normal {
            parts.push(format!("--kind {}", self.kind));
        }
        if !self.memo.is_empty() {
            parts.push(format!("--memo {}", shell_quote(&self.memo)));
        }
        for entry in &self.entries {
            let class = classes
                .get(&entry.account)
                .ok_or_else(|| ReconstructError::UnknownAccount(entry.account.clone()))?;
            parts.push(format!(
                "--entry {}:{}:{}",
                shell_quote(&entry.account.to_string()),
                class_to_str(*class),
                entry.amount,
            ));
        }

        let mut out = String::new();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                out.push_str(" \\\n  ");
            }
            out.push_str(part);
        }
        out.push('\n');
        Ok(out)
    }
}

/// Render a sequence of transactions as a series of `ledger add` commands,
/// one per transaction, separated by blank lines. The input order is
/// preserved; the caller is responsible for sorting (e.g., by `posted_at`
/// from `storage::list_transactions`).
///
/// # Errors
///
/// Returns `ReconstructError::UnknownAccount` if any entry's account is not
/// present in `classes`.
pub fn render_all_add_commands(
    transactions: &[Transaction],
    classes: &HashMap<AccountId, AccountClass>,
) -> Result<String, ReconstructError> {
    let mut out = String::new();
    for tx in transactions {
        out.push_str(&tx.render_add_command(classes)?);
        out.push('\n');
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconstructError {
    UnknownAccount(AccountId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseInfo {
    pub id: TransactionId,
    pub posted_at: Timestamp,
}

/// Every fiscal-year close in the book, in posted order.
///
/// Reads the recorded kind rather than inferring it from entry structure,
/// so a close stays a close regardless of how it sorts against same-day
/// transactions.
#[must_use]
pub fn closes(transactions: &[Transaction]) -> Vec<CloseInfo> {
    transactions
        .iter()
        .filter(|tx| tx.kind == TransactionKind::Close)
        .map(|tx| CloseInfo {
            id: tx.id,
            posted_at: tx.posted_at,
        })
        .collect()
}

/// When the current fiscal period began: the most recent close or open.
///
/// Both count. A close ends the previous period and starts this one; an open
/// starts the first period and means nothing precedes it. `None` is a book
/// with neither, whose period therefore runs over all of history.
#[must_use]
pub fn period_start(transactions: &[Transaction]) -> Option<Timestamp> {
    transactions
        .iter()
        .rfind(|tx| tx.kind.is_boundary())
        .map(|tx| tx.posted_at)
}

/// Income and expense flow over some period, as positive magnitudes:
/// `income` is what came in, `expenses` is what went out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    pub income: Decimal,
    pub expenses: Decimal,
}

impl Activity {
    #[must_use]
    pub fn net(&self) -> Decimal {
        self.income - self.expenses
    }
}

/// Book-level totals rolled up from account balances: the position half of
/// the picture, plus whatever activity the nominal accounts currently hold.
///
/// Balances arrive in the ledger's signed convention (positive means the
/// account went up) and leave in the one a reader expects: activity is
/// reported as positive magnitudes, while `liabilities` stays negative
/// while money is owed, so `assets + liabilities == net_worth()`.
///
/// Because a close zeroes the nominal accounts, `activity` covers only the
/// period since the last close — the fiscal year to date. For the whole
/// history see [`lifetime_activity`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub assets: Decimal,
    pub liabilities: Decimal,
    pub equity: Decimal,
    pub activity: Activity,
}

impl Summary {
    #[must_use]
    pub fn from_balances<I>(balances: I) -> Self
    where
        I: IntoIterator<Item = (AccountClass, Decimal)>,
    {
        let mut summary = Self::default();
        for (class, balance) in balances {
            match class {
                AccountClass::Asset => summary.assets += balance,
                AccountClass::Liability => summary.liabilities += balance,
                AccountClass::Equity => summary.equity += balance,
                AccountClass::Income => summary.activity.income -= balance,
                AccountClass::Expense => summary.activity.expenses += balance,
            }
        }
        summary
    }

    #[must_use]
    pub fn net_worth(&self) -> Decimal {
        self.assets + self.liabilities
    }
}

/// Cumulative income and expense flow across the whole history.
///
/// Balances alone cannot answer this once a year has been closed: a close
/// posts nominal entries that exactly offset the period it closes, so the
/// accounts read zero. This walks the transactions instead, counting only
/// `Normal` ones so the periods the boundaries retired still show up.
///
#[must_use]
pub fn lifetime_activity(
    transactions: &[Transaction],
    classes: &HashMap<AccountId, AccountClass>,
) -> Activity {
    let mut activity = Activity::default();
    for tx in transactions
        .iter()
        .filter(|tx| tx.kind == TransactionKind::Normal)
    {
        for entry in &tx.entries {
            match classes.get(&entry.account) {
                Some(AccountClass::Income) => activity.income -= entry.amount,
                Some(AccountClass::Expense) => activity.expenses += entry.amount,
                _ => {}
            }
        }
    }
    activity
}

impl std::fmt::Display for ReconstructError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAccount(id) => write!(f, "unknown account: {id}"),
        }
    }
}

impl std::error::Error for ReconstructError {}

fn class_to_str(class: AccountClass) -> &'static str {
    match class {
        AccountClass::Asset => "asset",
        AccountClass::Liability => "liability",
        AccountClass::Equity => "equity",
        AccountClass::Income => "income",
        AccountClass::Expense => "expense",
    }
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
    {
        return s.to_string();
    }
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use jiff::civil::Date;

    use super::*;

    fn ts(s: &str) -> Timestamp {
        Date::strptime("%Y-%m-%d", s)
            .expect("date parses")
            .at(0, 0, 0, 0)
            .to_zoned(TimeZone::UTC)
            .expect("midnight UTC is never ambiguous")
            .timestamp()
    }

    fn entry(account: &str, amount: &str) -> Entry {
        Entry {
            account: AccountId::parse(account).expect("account parses"),
            amount: Decimal::from_str(amount).expect("amount parses"),
        }
    }

    fn tx(memo: &str, entries: Vec<Entry>) -> Transaction {
        Transaction::new(
            TransactionId::new(),
            ts("2024-01-15"),
            memo.to_string(),
            entries,
        )
        .expect("tx balances")
    }

    fn dated(day: &str, memo: &str, entries: Vec<Entry>) -> Transaction {
        Transaction::new(TransactionId::new(), ts(day), memo.to_string(), entries)
            .expect("tx balances")
    }

    fn boundary(
        day: &str,
        kind: TransactionKind,
        entries: Vec<Entry>,
        classes: &HashMap<AccountId, AccountClass>,
    ) -> Transaction {
        dated(day, "", entries)
            .with_kind(kind, classes)
            .expect("boundary shape is valid")
    }

    fn classes(pairs: &[(&str, AccountClass)]) -> HashMap<AccountId, AccountClass> {
        pairs
            .iter()
            .map(|(id, class)| (AccountId::parse(id).expect("account parses"), *class))
            .collect()
    }

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).expect("amount parses")
    }

    /// A book mid-year: no close has happened yet, so equity is still zero
    /// and every dollar of net income is sitting in the asset accounts.
    fn open_book() -> Vec<(AccountClass, Decimal)> {
        vec![
            (AccountClass::Asset, dec("1500")),
            (AccountClass::Liability, dec("-265.44")),
            (AccountClass::Equity, dec("0")),
            (AccountClass::Income, dec("-2000")),
            (AccountClass::Expense, dec("765.44")),
        ]
    }

    /// The same book after a prior year closed 1000 of net income into
    /// `equity/net`, which the signed convention records as a negative.
    fn closed_book() -> Vec<(AccountClass, Decimal)> {
        vec![
            (AccountClass::Asset, dec("2500")),
            (AccountClass::Liability, dec("-265.44")),
            (AccountClass::Equity, dec("-1000")),
            (AccountClass::Income, dec("-2000")),
            (AccountClass::Expense, dec("765.44")),
        ]
    }

    #[test]
    fn summary_reports_income_and_expenses_as_magnitudes() {
        let summary = Summary::from_balances(open_book());
        assert_eq!(summary.assets, dec("1500"));
        assert_eq!(summary.liabilities, dec("-265.44"));
        assert_eq!(
            summary.activity.income,
            dec("2000"),
            "income flips to positive"
        );
        assert_eq!(
            summary.activity.expenses,
            dec("765.44"),
            "expenses stay positive"
        );
        assert_eq!(summary.net_worth(), dec("1234.56"));
        assert_eq!(summary.activity.net(), dec("1234.56"));
    }

    #[test]
    fn summary_sums_multiple_accounts_per_class() {
        let summary = Summary::from_balances(vec![
            (AccountClass::Asset, dec("1000")),
            (AccountClass::Asset, dec("500")),
            (AccountClass::Expense, dec("300")),
            (AccountClass::Expense, dec("465.44")),
        ]);
        assert_eq!(summary.assets, dec("1500"));
        assert_eq!(summary.activity.expenses, dec("765.44"));
    }

    #[test]
    fn summary_of_an_empty_book_is_all_zero() {
        let summary = Summary::from_balances(vec![]);
        assert_eq!(summary, Summary::default());
        assert_eq!(summary.net_worth(), Decimal::ZERO);
        assert_eq!(summary.activity.net(), Decimal::ZERO);
    }

    /// The accounting identity, restated in the summary's terms: every
    /// dollar of net worth is either this period's net income or a prior
    /// period's, parked in equity by a close.
    #[test]
    fn summary_satisfies_the_accounting_identity() {
        for balances in [open_book(), closed_book()] {
            let summary = Summary::from_balances(balances);
            assert_eq!(
                summary.net_worth() + summary.equity,
                summary.activity.net(),
                "identity broken for {summary:?}"
            );
        }
    }

    /// A close moves net income out of the nominal accounts and into
    /// equity without touching net worth.
    #[test]
    fn summary_net_worth_survives_a_close() {
        let before = Summary::from_balances(closed_book());
        let after = Summary::from_balances(vec![
            (AccountClass::Asset, dec("2500")),
            (AccountClass::Liability, dec("-265.44")),
            (AccountClass::Equity, dec("-2234.56")),
            (AccountClass::Income, dec("0")),
            (AccountClass::Expense, dec("0")),
        ]);
        assert_eq!(before.net_worth(), after.net_worth());
        assert_eq!(after.activity.net(), Decimal::ZERO);
    }

    fn fy_book() -> (Vec<Transaction>, HashMap<AccountId, AccountClass>) {
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("salary", AccountClass::Income),
            ("rent", AccountClass::Expense),
            ("equity/net", AccountClass::Equity),
        ]);
        let transactions = vec![
            tx(
                "Paycheck",
                vec![entry("checking", "1000"), entry("salary", "-1000")],
            ),
            tx(
                "Rent",
                vec![entry("checking", "-400"), entry("rent", "400")],
            ),
            dated(
                "2024-01-15",
                "FY close",
                vec![
                    entry("salary", "1000"),
                    entry("rent", "-400"),
                    entry("equity/net", "-600"),
                ],
            )
            .with_kind(TransactionKind::Close, &classes)
            .expect("close shape is valid"),
            tx(
                "Paycheck",
                vec![entry("checking", "1200"), entry("salary", "-1200")],
            ),
        ];
        (transactions, classes)
    }

    /// The close is skipped, so the year it retired still counts.
    #[test]
    fn lifetime_activity_counts_through_closes() {
        let (transactions, classes) = fy_book();
        assert_eq!(closes(&transactions).len(), 1, "close not marked");

        let lifetime = lifetime_activity(&transactions, &classes);
        assert_eq!(lifetime.income, dec("2200"), "both paychecks");
        assert_eq!(lifetime.expenses, dec("400"));
        assert_eq!(lifetime.net(), dec("1800"));
    }

    /// The balance-derived activity only sees the period since the close,
    /// which is exactly what makes the two views worth toggling between.
    #[test]
    fn lifetime_activity_exceeds_the_current_fiscal_year() {
        let (transactions, classes) = fy_book();
        let lifetime = lifetime_activity(&transactions, &classes);

        let mut balances: HashMap<AccountId, Decimal> = HashMap::new();
        for tx in &transactions {
            for entry in &tx.entries {
                *balances.entry(entry.account.clone()).or_default() += entry.amount;
            }
        }
        let fiscal_year = Summary::from_balances(
            balances
                .iter()
                .filter_map(|(id, balance)| classes.get(id).map(|class| (*class, *balance))),
        );

        assert_eq!(fiscal_year.activity.income, dec("1200"), "since the close");
        assert_eq!(fiscal_year.activity.expenses, Decimal::ZERO);
        assert_eq!(lifetime.net(), dec("1800"));
        assert_eq!(fiscal_year.activity.net(), dec("1200"));
        assert_eq!(
            fiscal_year.net_worth(),
            dec("1800"),
            "net worth is a position, unaffected by which period you view"
        );
    }

    /// With no close on the books the two views agree.
    #[test]
    fn lifetime_activity_matches_balances_before_any_close() {
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("salary", AccountClass::Income),
            ("rent", AccountClass::Expense),
        ]);
        let transactions = vec![
            tx(
                "Paycheck",
                vec![entry("checking", "1000"), entry("salary", "-1000")],
            ),
            tx(
                "Rent",
                vec![entry("checking", "-400"), entry("rent", "400")],
            ),
        ];
        assert!(closes(&transactions).is_empty());

        let lifetime = lifetime_activity(&transactions, &classes);
        assert_eq!(lifetime.income, dec("1000"));
        assert_eq!(lifetime.expenses, dec("400"));
    }

    /// Opening balances are asset/liability plus an equity plug, so they
    /// never register as income — "since open" and "all time" agree until
    /// the first close.
    #[test]
    fn lifetime_activity_ignores_opening_balances() {
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("equity/net", AccountClass::Equity),
            ("salary", AccountClass::Income),
        ]);
        let transactions = vec![
            tx(
                "Opening balances",
                vec![entry("checking", "5000"), entry("equity/net", "-5000")],
            ),
            tx(
                "Paycheck",
                vec![entry("checking", "1000"), entry("salary", "-1000")],
            ),
        ];
        let lifetime = lifetime_activity(&transactions, &classes);
        assert_eq!(lifetime.income, dec("1000"), "the plug is not income");
        assert_eq!(lifetime.expenses, Decimal::ZERO);
    }

    #[test]
    fn render_two_entry_paycheck() {
        let transaction = tx(
            "Paycheck",
            vec![entry("checking", "1000"), entry("income", "-1000")],
        );
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("income", AccountClass::Equity),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        assert_eq!(
            cmd,
            "ledger add --date 2024-01-15 \\\n  --memo Paycheck \\\n  --entry checking:asset:1000 \\\n  --entry income:equity:-1000\n"
        );
    }

    #[test]
    fn render_income_class_entry() {
        let transaction = tx(
            "Paycheck",
            vec![entry("checking", "1000"), entry("income", "-1000")],
        );
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("income", AccountClass::Income),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        assert!(cmd.contains("--entry income:income:-1000"), "got: {cmd}");
    }

    #[test]
    fn render_omits_memo_when_empty() {
        let transaction = tx(
            "",
            vec![entry("checking", "1000"), entry("income", "-1000")],
        );
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("income", AccountClass::Equity),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        assert_eq!(
            cmd,
            "ledger add --date 2024-01-15 \\\n  --entry checking:asset:1000 \\\n  --entry income:equity:-1000\n"
        );
    }

    #[test]
    fn render_quotes_memo_with_spaces() {
        let transaction = tx(
            "Paycheck January",
            vec![entry("checking", "500"), entry("income", "-500")],
        );
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("income", AccountClass::Equity),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        assert!(cmd.contains("'Paycheck January'"), "got: {cmd}");
    }

    #[test]
    fn render_quotes_account_id_with_spaces() {
        let transaction = tx(
            "",
            vec![entry("cash on hand", "100"), entry("income", "-100")],
        );
        let classes = classes(&[
            ("cash on hand", AccountClass::Asset),
            ("income", AccountClass::Equity),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        assert!(cmd.contains("'cash on hand':asset:100"), "got: {cmd}");
    }

    #[test]
    fn render_escapes_single_quote_in_memo() {
        let transaction = tx(
            "Bob's rent",
            vec![entry("checking", "100"), entry("rent", "-100")],
        );
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("rent", AccountClass::Expense),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        assert!(cmd.contains("'Bob'\\''s rent'"), "got: {cmd}");
    }

    #[test]
    fn render_preserves_decimal_amounts_without_trailing_zeros() {
        let transaction = tx(
            "",
            vec![entry("checking", "12.5"), entry("income", "-12.5")],
        );
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("income", AccountClass::Equity),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        assert!(cmd.contains("--entry checking:asset:12.5"), "got: {cmd}");
    }

    #[test]
    fn render_handles_multi_entry_with_continuations() {
        let transaction = tx(
            "split",
            vec![
                entry("expenses/dining", "80"),
                entry("checking", "-60"),
                entry("receivable/alex", "-20"),
            ],
        );
        let classes = classes(&[
            ("expenses/dining", AccountClass::Expense),
            ("checking", AccountClass::Asset),
            ("receivable/alex", AccountClass::Asset),
        ]);
        let cmd = transaction.render_add_command(&classes).expect("ok");
        let expected = "ledger add --date 2024-01-15 \\\n  --memo split \\\n  --entry expenses/dining:expense:80 \\\n  --entry checking:asset:-60 \\\n  --entry receivable/alex:asset:-20\n";
        assert_eq!(cmd, expected);
    }

    #[test]
    fn render_errors_on_unknown_account() {
        let transaction = tx("", vec![entry("checking", "100"), entry("ghost", "-100")]);
        let classes = classes(&[("checking", AccountClass::Asset)]);
        let err = transaction
            .render_add_command(&classes)
            .expect_err("missing class");
        assert_eq!(
            err,
            ReconstructError::UnknownAccount(AccountId::parse("ghost").unwrap())
        );
    }

    #[test]
    fn render_all_emits_each_command_in_input_order() {
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("income", AccountClass::Equity),
        ]);
        let transactions = vec![
            tx(
                "first",
                vec![entry("checking", "100"), entry("income", "-100")],
            ),
            tx(
                "second",
                vec![entry("checking", "200"), entry("income", "-200")],
            ),
        ];
        let out = render_all_add_commands(&transactions, &classes).expect("ok");
        let first_pos = out.find("first").expect("first present");
        let second_pos = out.find("second").expect("second present");
        assert!(first_pos < second_pos);
        assert!(
            out.contains("--entry income:equity:-100\n\nledger add"),
            "got: {out}"
        );
    }

    #[test]
    fn render_all_empty_input_is_empty_string() {
        let classes = classes(&[]);
        let out = render_all_add_commands(&[], &classes).expect("ok");
        assert_eq!(out, "");
    }

    #[test]
    fn render_all_propagates_first_error() {
        let classes = classes(&[
            ("checking", AccountClass::Asset),
            ("income", AccountClass::Equity),
        ]);
        let transactions = vec![
            tx(
                "ok",
                vec![entry("checking", "100"), entry("income", "-100")],
            ),
            tx(
                "broken",
                vec![entry("checking", "50"), entry("ghost", "-50")],
            ),
        ];
        let err = render_all_add_commands(&transactions, &classes).expect_err("missing class");
        assert_eq!(
            err,
            ReconstructError::UnknownAccount(AccountId::parse("ghost").unwrap())
        );
    }

    fn fy_classes() -> HashMap<AccountId, AccountClass> {
        classes(&[
            ("checking", AccountClass::Asset),
            ("salary", AccountClass::Income),
            ("expenses/food", AccountClass::Expense),
            ("equity/net", AccountClass::Equity),
        ])
    }

    #[test]
    fn closes_returns_close_kind_in_posted_order() {
        let classes = fy_classes();
        let first = boundary(
            "2024-12-31",
            TransactionKind::Close,
            vec![entry("salary", "1000"), entry("equity/net", "-1000")],
            &classes,
        );
        let second = boundary(
            "2025-12-31",
            TransactionKind::Close,
            vec![entry("salary", "500"), entry("equity/net", "-500")],
            &classes,
        );
        let (first_id, second_id) = (first.id, second.id);
        let transactions = vec![
            tx(
                "",
                vec![entry("checking", "1000"), entry("salary", "-1000")],
            ),
            first,
            tx("", vec![entry("checking", "500"), entry("salary", "-500")]),
            second,
        ];

        let found = closes(&transactions);
        assert_eq!(
            found,
            vec![
                CloseInfo {
                    id: first_id,
                    posted_at: ts("2024-12-31")
                },
                CloseInfo {
                    id: second_id,
                    posted_at: ts("2025-12-31")
                },
            ]
        );
    }

    #[test]
    fn closes_ignores_normal_and_open_transactions() {
        let classes = fy_classes();
        let transactions = vec![
            boundary(
                "2024-01-01",
                TransactionKind::Open,
                vec![entry("checking", "100"), entry("equity/net", "-100")],
                &classes,
            ),
            tx("", vec![entry("checking", "200"), entry("salary", "-200")]),
        ];
        assert_eq!(closes(&transactions), vec![]);
    }

    /// The reason kinds are recorded rather than inferred. These two
    /// transactions share a date, so their relative order is decided by
    /// ULID and is not stable — but the close is still a close either way.
    #[test]
    fn a_close_survives_same_day_ordering() {
        let classes = fy_classes();
        let closing = boundary(
            "2024-12-31",
            TransactionKind::Close,
            vec![entry("salary", "1000"), entry("equity/net", "-1000")],
            &classes,
        );
        let same_day = dated(
            "2024-12-31",
            "late paycheck",
            vec![entry("checking", "1000"), entry("salary", "-1000")],
        );

        let close_first = vec![closing.clone(), same_day.clone()];
        let close_last = vec![same_day, closing];
        assert_eq!(closes(&close_first).len(), 1);
        assert_eq!(closes(&close_last).len(), 1);
        assert_eq!(period_start(&close_first), Some(ts("2024-12-31")));
        assert_eq!(period_start(&close_last), Some(ts("2024-12-31")));
    }

    #[test]
    fn period_start_takes_the_latest_boundary() {
        let classes = fy_classes();
        let transactions = vec![
            boundary(
                "2024-01-01",
                TransactionKind::Open,
                vec![entry("checking", "100"), entry("equity/net", "-100")],
                &classes,
            ),
            tx("", vec![entry("checking", "200"), entry("salary", "-200")]),
            boundary(
                "2024-12-31",
                TransactionKind::Close,
                vec![entry("salary", "200"), entry("equity/net", "-200")],
                &classes,
            ),
            tx("", vec![entry("checking", "300"), entry("salary", "-300")]),
        ];
        assert_eq!(period_start(&transactions), Some(ts("2024-12-31")));
    }

    /// An open is a boundary too — it is where the first period starts.
    #[test]
    fn period_start_counts_an_open_when_nothing_has_closed() {
        let classes = fy_classes();
        let transactions = vec![
            boundary(
                "2024-01-01",
                TransactionKind::Open,
                vec![entry("checking", "100"), entry("equity/net", "-100")],
                &classes,
            ),
            tx("", vec![entry("checking", "200"), entry("salary", "-200")]),
        ];
        assert!(closes(&transactions).is_empty());
        assert_eq!(period_start(&transactions), Some(ts("2024-01-01")));
    }

    #[test]
    fn period_start_is_none_without_a_boundary() {
        let transactions = vec![tx(
            "",
            vec![entry("checking", "200"), entry("salary", "-200")],
        )];
        assert_eq!(period_start(&transactions), None);
        assert_eq!(period_start(&[]), None);
    }

    #[test]
    fn with_kind_accepts_the_shapes_close_and_open_actually_produce() {
        let classes = fy_classes();
        dated(
            "2024-12-31",
            "",
            vec![
                entry("salary", "1000"),
                entry("expenses/food", "-400"),
                entry("equity/net", "-600"),
            ],
        )
        .with_kind(TransactionKind::Close, &classes)
        .expect("nominal + equity is a close");

        dated(
            "2024-01-01",
            "",
            vec![entry("checking", "100"), entry("equity/net", "-100")],
        )
        .with_kind(TransactionKind::Open, &classes)
        .expect("real + equity is an open");
    }

    /// A close settles nominal balances into equity; touching an asset
    /// means it is a transfer, not a close.
    #[test]
    fn with_kind_rejects_a_close_that_touches_assets() {
        let classes = fy_classes();
        let err = dated(
            "2024-12-31",
            "",
            vec![entry("checking", "100"), entry("salary", "-100")],
        )
        .with_kind(TransactionKind::Close, &classes)
        .expect_err("assets are not allowed in a close");
        assert!(
            matches!(
                err,
                TransactionError::WrongShape {
                    kind: TransactionKind::Close,
                    class: AccountClass::Asset,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn with_kind_rejects_an_open_that_touches_nominals() {
        let classes = fy_classes();
        let err = dated(
            "2024-01-01",
            "",
            vec![entry("salary", "-100"), entry("checking", "100")],
        )
        .with_kind(TransactionKind::Open, &classes)
        .expect_err("income is not an opening balance");
        assert!(
            matches!(
                err,
                TransactionError::WrongShape {
                    kind: TransactionKind::Open,
                    class: AccountClass::Income,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn with_kind_rejects_an_unknown_account() {
        let err = dated(
            "2024-12-31",
            "",
            vec![entry("salary", "100"), entry("nowhere", "-100")],
        )
        .with_kind(TransactionKind::Close, &fy_classes())
        .expect_err("unregistered account");
        assert!(matches!(err, TransactionError::UnknownAccount(_)), "{err}");
    }

    /// Normal is the default and imposes no shape at all.
    #[test]
    fn transactions_are_normal_unless_marked() {
        let plain = tx("", vec![entry("checking", "100"), entry("salary", "-100")]);
        assert_eq!(plain.kind, TransactionKind::Normal);
        assert_eq!(TransactionKind::default(), TransactionKind::Normal);
        assert!(!TransactionKind::Normal.is_boundary());
        assert!(TransactionKind::Close.is_boundary());
        assert!(TransactionKind::Open.is_boundary());
    }

    #[test]
    fn kind_round_trips_through_its_string_form() {
        for kind in [
            TransactionKind::Normal,
            TransactionKind::Close,
            TransactionKind::Open,
        ] {
            assert_eq!(TransactionKind::parse(kind.as_str()), Ok(kind));
        }
        let err = TransactionKind::parse("banana").expect_err("not a kind");
        assert!(matches!(err, TransactionError::UnknownKind(_)), "{err}");
    }

    #[test]
    fn reconstruct_emits_the_kind_so_a_rebuild_keeps_it() {
        let classes = fy_classes();
        let closing = boundary(
            "2024-12-31",
            TransactionKind::Close,
            vec![entry("salary", "1000"), entry("equity/net", "-1000")],
            &classes,
        );
        let cmd = closing.render_add_command(&classes).expect("renders");
        assert!(cmd.contains("--kind close"), "{cmd}");

        let plain = tx("", vec![entry("checking", "100"), entry("salary", "-100")]);
        let cmd = plain.render_add_command(&classes).expect("renders");
        assert!(!cmd.contains("--kind"), "normal needs no flag: {cmd}");
    }
}
