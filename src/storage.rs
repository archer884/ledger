use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, params};
use rust_decimal::Decimal;
use thiserror::Error;
use ulid::Ulid;

use jiff::Timestamp;

use crate::model::{
    Account, AccountClass, AccountId, Entry, Transaction, TransactionError, TransactionId,
};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid account class in db: {0}")]
    InvalidClass(String),
    #[error("invalid ulid in db: {0}")]
    InvalidUlid(String),
    #[error("invalid timestamp in db: {0}")]
    InvalidTimestamp(String),
    #[error("invalid decimal in db: {0}")]
    InvalidDecimal(String),
    #[error("invalid account id in db: {0}")]
    InvalidAccountId(String),
    #[error("unknown account: {0}")]
    UnknownAccount(String),
    #[error("invalid transaction: {0}")]
    InvalidTransaction(#[from] TransactionError),
}

pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// # Errors
    ///
    /// Returns an error if `SQLite` cannot open the file at `path` or run
    /// the initial migration.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// # Errors
    ///
    /// Returns an error if `SQLite` cannot create an in-memory database or
    /// run the initial migration.
    pub fn in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, StorageError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// # Errors
    ///
    /// Returns an error if the underlying `SQLite` insert fails.
    pub fn register_account(&self, account: &Account) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO accounts (id, class) VALUES (?1, ?2)",
            params![account.id.as_str(), class_to_str(account.class)],
        )?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the underlying `SQLite` query fails or if any
    /// stored row fails to parse as a valid account.
    pub fn list_accounts(&self) -> Result<Vec<Account>, StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, class FROM accounts ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let class: String = row.get(1)?;
            Ok((id, class))
        })?;
        let mut accounts = Vec::new();
        for row in rows {
            let (id, class) = row?;
            accounts.push(Account {
                id: AccountId::parse(&id)
                    .map_err(|_| StorageError::InvalidAccountId(id.clone()))?,
                class: class_from_str(&class)?,
            });
        }
        Ok(accounts)
    }

    /// # Errors
    ///
    /// Returns `StorageError::UnknownAccount` if any entry references an
    /// account that has not been registered. Also returns an error if the
    /// underlying `SQLite` transaction or inserts fail.
    pub fn save_transaction(&self, tx: &Transaction) -> Result<(), StorageError> {
        for entry in &tx.entries {
            let exists: bool = self
                .conn
                .query_row(
                    "SELECT 1 FROM accounts WHERE id = ?1",
                    params![entry.account.as_str()],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !exists {
                return Err(StorageError::UnknownAccount(entry.account.to_string()));
            }
        }

        let tx_conn = self.conn.unchecked_transaction()?;
        tx_conn.execute(
            "INSERT INTO transactions (id, posted_at, memo) VALUES (?1, ?2, ?3)",
            params![tx.id.0.to_string(), tx.posted_at.to_string(), tx.memo],
        )?;
        for entry in &tx.entries {
            tx_conn.execute(
                "INSERT INTO entries (transaction_id, account, amount) VALUES (?1, ?2, ?3)",
                params![
                    tx.id.0.to_string(),
                    entry.account.as_str(),
                    entry.amount.to_string(),
                ],
            )?;
        }
        tx_conn.commit()?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the underlying `SQLite` query fails or if any
    /// stored row fails to parse back into a valid `Transaction`.
    pub fn list_transactions(&self) -> Result<Vec<Transaction>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.posted_at, t.memo, e.account, e.amount
             FROM transactions t
             JOIN entries e ON e.transaction_id = t.id
             ORDER BY t.posted_at, t.id, e.rowid",
        )?;

        let mut transactions: Vec<Transaction> = Vec::new();
        let mut current: Option<Transaction> = None;

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let posted_at: String = row.get(1)?;
            let memo: String = row.get(2)?;
            let account: String = row.get(3)?;
            let amount: String = row.get(4)?;
            Ok((id, posted_at, memo, account, amount))
        })?;

        for row in rows {
            let (id_str, posted_at_str, memo, account_str, amount_str) = row?;
            let id = Ulid::from_string(&id_str)
                .map_err(|_| StorageError::InvalidUlid(id_str.clone()))?;
            let posted_at = Timestamp::from_str(&posted_at_str)
                .map_err(|_| StorageError::InvalidTimestamp(posted_at_str.clone()))?;
            let account = AccountId::parse(&account_str)
                .map_err(|_| StorageError::InvalidAccountId(account_str.clone()))?;
            let amount = Decimal::from_str(&amount_str)
                .map_err(|_| StorageError::InvalidDecimal(amount_str.clone()))?;

            let entry = Entry { account, amount };

            let id = TransactionId(id);
            match current.as_mut() {
                Some(t) if t.id == id => t.entries.push(entry),
                _ => {
                    if let Some(t) = current.take() {
                        transactions.push(t);
                    }
                    current = Some(Transaction::from_raw(id, posted_at, memo, vec![entry]));
                }
            }
        }
        if let Some(t) = current {
            transactions.push(t);
        }
        Ok(transactions)
    }

    /// # Errors
    ///
    /// Returns an error if the underlying `SQLite` query fails or if any
    /// stored amount fails to parse as a `Decimal`.
    pub fn balances(&self) -> Result<HashMap<AccountId, Decimal>, StorageError> {
        let mut stmt = self.conn.prepare("SELECT account, amount FROM entries")?;
        let rows = stmt.query_map([], |row| {
            let account: String = row.get(0)?;
            let amount: String = row.get(1)?;
            Ok((account, amount))
        })?;

        let mut balances: HashMap<AccountId, Decimal> = HashMap::new();
        for row in rows {
            let (account_str, amount_str) = row?;
            let account = AccountId::parse(&account_str)
                .map_err(|_| StorageError::InvalidAccountId(account_str.clone()))?;
            let amount = Decimal::from_str(&amount_str)
                .map_err(|_| StorageError::InvalidDecimal(amount_str.clone()))?;
            *balances.entry(account).or_insert(Decimal::ZERO) += amount;
        }
        Ok(balances)
    }
}

fn class_to_str(class: AccountClass) -> &'static str {
    match class {
        AccountClass::Asset => "asset",
        AccountClass::Liability => "liability",
        AccountClass::Equity => "equity",
        AccountClass::Expense => "expense",
    }
}

fn class_from_str(s: &str) -> Result<AccountClass, StorageError> {
    match s {
        "asset" => Ok(AccountClass::Asset),
        "liability" => Ok(AccountClass::Liability),
        "equity" => Ok(AccountClass::Equity),
        "expense" => Ok(AccountClass::Expense),
        other => Err(StorageError::InvalidClass(other.to_string())),
    }
}

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY,
    class TEXT NOT NULL CHECK (class IN ('asset', 'liability', 'equity', 'expense'))
);

CREATE TABLE IF NOT EXISTS transactions (
    id TEXT PRIMARY KEY,
    posted_at TEXT NOT NULL,
    memo TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entries (
    transaction_id TEXT NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    account TEXT NOT NULL REFERENCES accounts(id),
    amount TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entries_account ON entries(account);
CREATE INDEX IF NOT EXISTS idx_entries_transaction ON entries(transaction_id);
";
