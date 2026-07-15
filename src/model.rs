use std::collections::HashMap;

use jiff::Timestamp;
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
        Self(Ulid::new())
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
}
