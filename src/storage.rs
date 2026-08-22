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
    TransactionKind,
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
    #[error("unknown transaction: {0}")]
    UnknownTransaction(String),
    #[error("invalid transaction: {0}")]
    InvalidTransaction(#[from] TransactionError),
    #[error("invalid transaction kind in db: {0}")]
    InvalidKind(String),
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
        Self::add_transaction_kind(&conn)?;
        Ok(Self { conn })
    }

    /// Databases written before transaction kinds existed have no `kind`
    /// column, and `CREATE TABLE IF NOT EXISTS` will not add one. Every
    /// existing row becomes `normal`: the old closes are not recoverable
    /// from the schema, so rebuild with `reconstruct --all` to restore them.
    fn add_transaction_kind(conn: &Connection) -> Result<(), StorageError> {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('transactions')")?;
        let mut has_kind = false;
        for name in stmt.query_map([], |row| row.get::<_, String>(0))? {
            if name? == "kind" {
                has_kind = true;
            }
        }
        drop(stmt);
        if !has_kind {
            conn.execute_batch(
                "ALTER TABLE transactions ADD COLUMN kind TEXT NOT NULL DEFAULT 'normal'
                 CHECK (kind IN ('normal', 'close', 'open'))",
            )?;
        }
        Ok(())
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
            "INSERT INTO transactions (id, posted_at, memo, kind) VALUES (?1, ?2, ?3, ?4)",
            params![
                tx.id.0.to_string(),
                tx.posted_at.to_string(),
                tx.memo,
                tx.kind.as_str(),
            ],
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
    /// Returns `StorageError::UnknownAccount` if any entry references an
    /// account that has not been registered. Also returns an error if the
    /// underlying `SQLite` transaction or the delete/insert fails.
    pub fn replace_transaction(&self, tx: &Transaction) -> Result<(), StorageError> {
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
            "DELETE FROM transactions WHERE id = ?1",
            params![tx.id.0.to_string()],
        )?;
        tx_conn.execute(
            "INSERT INTO transactions (id, posted_at, memo, kind) VALUES (?1, ?2, ?3, ?4)",
            params![
                tx.id.0.to_string(),
                tx.posted_at.to_string(),
                tx.memo,
                tx.kind.as_str(),
            ],
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
    /// Returns an error if the underlying `SQLite` delete fails. The
    /// schema's `ON DELETE CASCADE` removes the transaction's entries.
    pub fn delete_transaction(&self, id: TransactionId) -> Result<(), StorageError> {
        let changed = self.conn.execute(
            "DELETE FROM transactions WHERE id = ?1",
            params![id.0.to_string()],
        )?;
        if changed == 0 {
            return Err(StorageError::UnknownTransaction(id.0.to_string()));
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns `StorageError::UnknownTransaction` if no row matches `id`.
    /// Also returns an error if the underlying `SQLite` query fails or if any
    /// stored row fails to parse back into a valid `Transaction`.
    pub fn get_transaction(&self, id: TransactionId) -> Result<Transaction, StorageError> {
        let id_str = id.0.to_string();
        let (posted_at, memo, kind): (String, String, String) = self
            .conn
            .query_row(
                "SELECT posted_at, memo, kind FROM transactions WHERE id = ?1",
                params![id_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::UnknownTransaction(id_str.clone())
                }
                other => StorageError::Sqlite(other),
            })?;
        let posted_at = Timestamp::from_str(&posted_at)
            .map_err(|_| StorageError::InvalidTimestamp(posted_at.clone()))?;
        let kind = TransactionKind::parse(&kind).map_err(|_| StorageError::InvalidKind(kind))?;

        let mut stmt = self.conn.prepare(
            "SELECT account, amount FROM entries WHERE transaction_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![id_str], |row| {
            let account: String = row.get(0)?;
            let amount: String = row.get(1)?;
            Ok((account, amount))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (account_str, amount_str) = row?;
            let account = AccountId::parse(&account_str)
                .map_err(|_| StorageError::InvalidAccountId(account_str.clone()))?;
            let amount = Decimal::from_str(&amount_str)
                .map_err(|_| StorageError::InvalidDecimal(amount_str.clone()))?;
            entries.push(Entry { account, amount });
        }

        Ok(Transaction::from_raw(id, posted_at, memo, entries, kind))
    }

    /// # Errors
    ///
    /// Returns an error if the underlying `SQLite` query fails or if any
    /// stored row fails to parse back into a valid `Transaction`.
    pub fn list_transactions(&self) -> Result<Vec<Transaction>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.posted_at, t.memo, t.kind, e.account, e.amount
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
            let kind: String = row.get(3)?;
            let account: String = row.get(4)?;
            let amount: String = row.get(5)?;
            Ok((id, posted_at, memo, kind, account, amount))
        })?;

        for row in rows {
            let (id_str, posted_at_str, memo, kind_str, account_str, amount_str) = row?;
            let id = Ulid::from_string(&id_str)
                .map_err(|_| StorageError::InvalidUlid(id_str.clone()))?;
            let posted_at = Timestamp::from_str(&posted_at_str)
                .map_err(|_| StorageError::InvalidTimestamp(posted_at_str.clone()))?;
            let kind = TransactionKind::parse(&kind_str)
                .map_err(|_| StorageError::InvalidKind(kind_str.clone()))?;
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
                    current = Some(Transaction::from_raw(
                        id,
                        posted_at,
                        memo,
                        vec![entry],
                        kind,
                    ));
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
        AccountClass::Income => "income",
        AccountClass::Expense => "expense",
    }
}

fn class_from_str(s: &str) -> Result<AccountClass, StorageError> {
    match s {
        "asset" => Ok(AccountClass::Asset),
        "liability" => Ok(AccountClass::Liability),
        "equity" => Ok(AccountClass::Equity),
        "income" => Ok(AccountClass::Income),
        "expense" => Ok(AccountClass::Expense),
        other => Err(StorageError::InvalidClass(other.to_string())),
    }
}

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY,
    class TEXT NOT NULL CHECK (class IN ('asset', 'liability', 'equity', 'income', 'expense'))
);

CREATE TABLE IF NOT EXISTS transactions (
    id TEXT PRIMARY KEY,
    posted_at TEXT NOT NULL,
    memo TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'normal' CHECK (kind IN ('normal', 'close', 'open'))
);

CREATE TABLE IF NOT EXISTS entries (
    transaction_id TEXT NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    account TEXT NOT NULL REFERENCES accounts(id),
    amount TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entries_account ON entries(account);
CREATE INDEX IF NOT EXISTS idx_entries_transaction ON entries(transaction_id);
";

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::Date;
    use jiff::tz::TimeZone;

    fn posted(day: &str) -> Timestamp {
        Date::strptime("%Y-%m-%d", day)
            .expect("date parses")
            .at(0, 0, 0, 0)
            .to_zoned(TimeZone::UTC)
            .expect("midnight UTC is never ambiguous")
            .timestamp()
    }

    fn acct(storage: &Storage, id: &str, class: AccountClass) -> Account {
        let account = Account {
            id: AccountId::parse(id).expect("account parses"),
            class,
        };
        storage
            .register_account(&account)
            .expect("account registered");
        account
    }

    fn build_tx(
        id: TransactionId,
        day: &str,
        memo: &str,
        entries: Vec<(String, i64)>,
    ) -> Transaction {
        let entries = entries
            .into_iter()
            .map(|(a, amt)| Entry {
                account: AccountId::parse(&a).expect("account parses"),
                amount: Decimal::from(amt),
            })
            .collect();
        Transaction::new(id, posted(day), memo.to_string(), entries).expect("tx balances")
    }

    fn tx_balance(storage: &Storage, account: &str, tx_id: TransactionId) -> Decimal {
        storage
            .list_transactions()
            .expect("list works")
            .into_iter()
            .find(|t| t.id == tx_id)
            .expect("tx exists")
            .entries
            .into_iter()
            .filter(|e| e.account.as_str() == account)
            .map(|e| e.amount)
            .sum()
    }

    fn new_storage() -> Storage {
        let storage = Storage::in_memory().expect("in-memory db");
        acct(&storage, "checking", AccountClass::Asset);
        acct(&storage, "savings", AccountClass::Asset);
        acct(&storage, "income", AccountClass::Equity);
        acct(&storage, "groceries", AccountClass::Expense);
        storage
    }

    #[test]
    fn income_class_account_round_trips() {
        let storage = Storage::in_memory().expect("in-memory db");
        acct(&storage, "salary", AccountClass::Income);

        let accounts = storage.list_accounts().expect("list works");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id.as_str(), "salary");
        assert_eq!(accounts[0].class, AccountClass::Income);
    }

    #[test]
    fn delete_transaction_removes_it_and_cascades_entries() {
        let storage = new_storage();
        let id = TransactionId::new();
        let tx = build_tx(
            id,
            "2024-01-15",
            "paycheck",
            vec![("checking".to_string(), 500), ("income".to_string(), -500)],
        );
        storage.save_transaction(&tx).expect("saved");

        assert_eq!(storage.list_transactions().expect("list works").len(), 1);
        assert_eq!(tx_balance(&storage, "checking", id), Decimal::from(500));

        storage.delete_transaction(id).expect("deleted");
        assert_eq!(storage.list_transactions().expect("list works").len(), 0);
        // Cascading delete removed the entries, so the ledger balances back
        // to its pre-transaction empty state.
        let balances = storage.balances().expect("balances work");
        let total: Decimal = balances.values().copied().sum();
        assert_eq!(total, Decimal::ZERO);
        assert!(balances.is_empty());
    }

    #[test]
    fn delete_unknown_transaction_errors() {
        let storage = new_storage();
        let err = storage
            .delete_transaction(TransactionId::new())
            .expect_err("missing tx should error");
        assert!(matches!(err, StorageError::UnknownTransaction(_)));
    }

    #[test]
    fn replace_transaction_remaps_accounts_keeping_amounts_and_id() {
        let storage = new_storage();
        let id = TransactionId::new();
        let tx = build_tx(
            id,
            "2024-01-15",
            "paycheck",
            vec![("checking".to_string(), 500), ("income".to_string(), -500)],
        );
        storage.save_transaction(&tx).expect("saved");

        // Re-route the income side to savings instead; amounts unchanged so
        // the zero-sum invariant holds.
        let replaced = build_tx(
            id,
            "2024-01-15",
            "paycheck",
            vec![("checking".to_string(), 500), ("savings".to_string(), -500)],
        );
        storage.replace_transaction(&replaced).expect("replaced");

        let listed = storage.list_transactions().expect("list works");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].memo, "paycheck");
        // income no longer involved; savings now holds the credit.
        assert_eq!(tx_balance(&storage, "income", id), Decimal::ZERO);
        assert_eq!(tx_balance(&storage, "savings", id), Decimal::from(-500));
        assert_eq!(tx_balance(&storage, "checking", id), Decimal::from(500));
    }

    #[test]
    fn replace_transaction_rejects_unknown_account() {
        let storage = new_storage();
        let id = TransactionId::new();
        let tx = build_tx(
            id,
            "2024-01-15",
            "paycheck",
            vec![("checking".to_string(), 500), ("income".to_string(), -500)],
        );
        storage.save_transaction(&tx).expect("saved");

        let replaced = build_tx(
            id,
            "2024-01-15",
            "paycheck",
            vec![("checking".to_string(), 500), ("ghost".to_string(), -500)],
        );
        let err = storage
            .replace_transaction(&replaced)
            .expect_err("ghost account should be rejected");
        assert!(matches!(err, StorageError::UnknownAccount(_)));
        // Original transaction is untouched because validation runs before
        // the delete/insert.
        let listed = storage.list_transactions().expect("list works");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(tx_balance(&storage, "income", id), Decimal::from(-500));
    }

    #[test]
    fn get_transaction_returns_saved_transaction() {
        let storage = new_storage();
        let id = TransactionId::new();
        let tx = build_tx(
            id,
            "2024-01-15",
            "paycheck",
            vec![("checking".to_string(), 500), ("income".to_string(), -500)],
        );
        storage.save_transaction(&tx).expect("saved");

        let got = storage.get_transaction(id).expect("found");
        assert_eq!(got.id, id);
        assert_eq!(got.posted_at, posted("2024-01-15"));
        assert_eq!(got.memo, "paycheck");
        assert_eq!(got.entries.len(), 2);
        assert_eq!(got.entries[0].account.as_str(), "checking");
        assert_eq!(got.entries[0].amount, Decimal::from(500));
        assert_eq!(got.entries[1].account.as_str(), "income");
        assert_eq!(got.entries[1].amount, Decimal::from(-500));
    }

    #[test]
    fn get_transaction_preserves_entry_order_within_a_transaction() {
        let storage = new_storage();
        let id = TransactionId::new();
        // Deliberately non-alphabetic order; the rowid-ordered read should
        // mirror the insert order, not sort by account.
        let tx = build_tx(
            id,
            "2024-01-15",
            "split",
            vec![
                ("groceries".to_string(), 60),
                ("checking".to_string(), -50),
                ("savings".to_string(), -10),
            ],
        );
        storage.save_transaction(&tx).expect("saved");

        let got = storage.get_transaction(id).expect("found");
        let order: Vec<&str> = got.entries.iter().map(|e| e.account.as_str()).collect();
        assert_eq!(order, vec!["groceries", "checking", "savings"]);
    }

    #[test]
    fn get_transaction_unknown_id_errors() {
        let storage = new_storage();
        let err = storage
            .get_transaction(TransactionId::new())
            .expect_err("missing tx should error");
        assert!(matches!(err, StorageError::UnknownTransaction(_)));
    }
}
