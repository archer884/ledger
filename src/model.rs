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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub id: TransactionId,
    pub posted_at: Timestamp,
    pub memo: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionError {
    TooFewEntries { got: usize, min: usize },
    Unbalanced { sum: Decimal },
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
        })
    }

    pub(crate) fn from_raw(
        id: TransactionId,
        posted_at: Timestamp,
        memo: String,
        entries: Vec<Entry>,
    ) -> Self {
        Self {
            id,
            posted_at,
            memo,
            entries,
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
        let mut parts: Vec<String> = Vec::with_capacity(2 + self.entries.len());
        parts.push(format!("ledger add --date {date}"));
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

/// Identify the fiscal-year closing transactions in a ledger.
///
/// A transaction is a close iff it posts to at least one income or
/// expense account, every one of its remaining entries posts to an
/// equity account, and after applying it every income and expense
/// account in `classes` has an all-time balance of exactly zero.
/// `transactions` must be in posted order, as returned by
/// `storage::list_transactions`; balances are tracked across the whole
/// slice, so closes buried in history are still recognized.
#[must_use]
pub fn detect_closes(
    transactions: &[Transaction],
    classes: &HashMap<AccountId, AccountClass>,
) -> Vec<CloseInfo> {
    let mut balances: HashMap<AccountId, Decimal> = HashMap::new();
    let mut closes = Vec::new();

    for tx in transactions {
        let mut touches_nominal = false;
        let mut others_all_equity = true;
        for entry in &tx.entries {
            match classes.get(&entry.account) {
                Some(AccountClass::Income | AccountClass::Expense) => {
                    touches_nominal = true;
                }
                Some(AccountClass::Equity) => {}
                Some(AccountClass::Asset | AccountClass::Liability) | None => {
                    others_all_equity = false;
                }
            }
        }

        for entry in &tx.entries {
            *balances
                .entry(entry.account.clone())
                .or_insert(Decimal::ZERO) += entry.amount;
        }

        if !touches_nominal || !others_all_equity {
            continue;
        }

        let nominals_zero = classes
            .iter()
            .filter(|(_, class)| matches!(class, AccountClass::Income | AccountClass::Expense))
            .all(|(id, _)| balances.get(id).is_none_or(|b| *b == Decimal::ZERO));
        if nominals_zero {
            closes.push(CloseInfo {
                id: tx.id,
                posted_at: tx.posted_at,
            });
        }
    }
    closes
}

/// Book-level totals rolled up from account balances.
///
/// Balances arrive in the ledger's signed convention (positive means the
/// account went up) and leave in the one a reader expects: `income` and
/// `expenses` are positive magnitudes, while `liabilities` stays negative
/// while money is owed, so both reported columns sum on their own —
/// `assets + liabilities == net_worth()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub assets: Decimal,
    pub liabilities: Decimal,
    pub equity: Decimal,
    pub income: Decimal,
    pub expenses: Decimal,
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
                AccountClass::Income => summary.income -= balance,
                AccountClass::Expense => summary.expenses += balance,
            }
        }
        summary
    }

    #[must_use]
    pub fn net_worth(&self) -> Decimal {
        self.assets + self.liabilities
    }

    #[must_use]
    pub fn net_income(&self) -> Decimal {
        self.income - self.expenses
    }
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
        assert_eq!(summary.income, dec("2000"), "income flips to positive");
        assert_eq!(summary.expenses, dec("765.44"), "expenses stay positive");
        assert_eq!(summary.net_worth(), dec("1234.56"));
        assert_eq!(summary.net_income(), dec("1234.56"));
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
        assert_eq!(summary.expenses, dec("765.44"));
    }

    #[test]
    fn summary_of_an_empty_book_is_all_zero() {
        let summary = Summary::from_balances(vec![]);
        assert_eq!(summary, Summary::default());
        assert_eq!(summary.net_worth(), Decimal::ZERO);
        assert_eq!(summary.net_income(), Decimal::ZERO);
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
                summary.net_income(),
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
        assert_eq!(after.net_income(), Decimal::ZERO);
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

    fn close(day: &str, entries: Vec<Entry>) -> Transaction {
        Transaction::new(TransactionId::new(), ts(day), String::new(), entries)
            .expect("close balances")
    }

    fn fy_classes() -> HashMap<AccountId, AccountClass> {
        classes(&[
            ("checking", AccountClass::Asset),
            ("salary", AccountClass::Income),
            ("expenses/food", AccountClass::Expense),
            ("equity/net", AccountClass::Equity),
        ])
    }

    fn running_balances(transactions: &[Transaction]) -> HashMap<AccountId, Decimal> {
        let mut all: HashMap<AccountId, Decimal> = HashMap::new();
        for t in transactions {
            for (account, delta) in t.balance_by_account() {
                *all.entry(account).or_insert(Decimal::ZERO) += delta;
            }
        }
        all
    }

    #[test]
    fn detect_closes_finds_a_proper_close() {
        let closing = close(
            "2024-12-31",
            vec![
                entry("salary", "1000"),
                entry("expenses/food", "-100"),
                entry("equity/net", "-900"),
            ],
        );
        let transactions = vec![
            tx(
                "",
                vec![entry("checking", "1000"), entry("salary", "-1000")],
            ),
            tx(
                "",
                vec![entry("expenses/food", "100"), entry("checking", "-100")],
            ),
            closing.clone(),
        ];
        let closes = detect_closes(&transactions, &fy_classes());
        assert_eq!(
            closes,
            vec![CloseInfo {
                id: closing.id,
                posted_at: ts("2024-12-31")
            }]
        );

        let balances = running_balances(&transactions);
        assert_eq!(
            balances[&AccountId::parse("salary").unwrap()],
            Decimal::ZERO
        );
        assert_eq!(
            balances[&AccountId::parse("expenses/food").unwrap()],
            Decimal::ZERO
        );
        let total: Decimal = balances.values().copied().sum();
        assert_eq!(total, Decimal::ZERO);
    }

    #[test]
    fn detect_closes_ignores_open_and_paycheck() {
        let classes = fy_classes();
        let transactions = vec![
            tx(
                "",
                vec![entry("checking", "100"), entry("equity/net", "-100")],
            ),
            tx("", vec![entry("checking", "200"), entry("salary", "-200")]),
        ];
        assert_eq!(detect_closes(&transactions, &classes), vec![]);
    }

    #[test]
    fn detect_closes_rejects_partial_zeroing() {
        // salary is zeroed but expenses/food still carries +100.
        let classes = fy_classes();
        let transactions = vec![
            tx(
                "",
                vec![entry("checking", "1000"), entry("salary", "-1000")],
            ),
            tx(
                "",
                vec![entry("expenses/food", "100"), entry("checking", "-100")],
            ),
            tx(
                "",
                vec![entry("salary", "1000"), entry("equity/net", "-1000")],
            ),
        ];
        assert_eq!(detect_closes(&transactions, &classes), vec![]);
    }

    #[test]
    fn detect_closes_rejects_asset_entries() {
        let classes = fy_classes();
        let transactions = vec![
            tx("", vec![entry("checking", "100"), entry("salary", "-100")]),
            tx("", vec![entry("salary", "100"), entry("checking", "-100")]),
        ];
        assert_eq!(detect_closes(&transactions, &classes), vec![]);
    }

    #[test]
    fn detect_closes_finds_consecutive_closes() {
        let classes = fy_classes();
        let first = close(
            "2024-12-31",
            vec![entry("salary", "1000"), entry("equity/net", "-1000")],
        );
        let second = close(
            "2025-12-31",
            vec![entry("salary", "500"), entry("equity/net", "-500")],
        );
        let first_id = first.id;
        let second_id = second.id;
        let transactions = vec![
            tx(
                "",
                vec![entry("checking", "1000"), entry("salary", "-1000")],
            ),
            first,
            tx("", vec![entry("checking", "500"), entry("salary", "-500")]),
            second,
        ];
        let closes = detect_closes(&transactions, &classes);
        assert_eq!(closes.len(), 2);
        assert_eq!(closes[0].posted_at, ts("2024-12-31"));
        assert_eq!(closes[1].posted_at, ts("2025-12-31"));
        assert_eq!(closes[0].id, first_id);
        assert_eq!(closes[1].id, second_id);
    }
}
