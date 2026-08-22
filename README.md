# ledger

A personal financial ledger: double-entry accounting in your terminal, stored in a local SQLite file. CLI for writing transactions, TUI for browsing them.

## Quick start

```bash
cargo run -- add --date 2026-01-15 --memo "Paycheck" \
  --entry checking:asset:1000 \
  --entry income:income:-1000

cargo run -- income 500 --memo "Bonus"

cargo run            # launches the TUI on the default database
cargo run -- tui     # explicit
```

## Usage

### Subcommands

| Command | Purpose |
|---|---|
| `ledger add` | Add a transaction with arbitrary entries |
| `ledger income <amount>` | Shortcut: debit `checking`, credit `income` |
| `ledger open` | Record opening balances on an empty database (plugs the difference into `equity/net`) |
| `ledger close` | Close the fiscal year: zero every income and expense account into `equity/net` |
| `ledger audit --from D --to D` | Receipts / disbursements report for a date range |
| `ledger audit --fy YEAR\|latest` | Same report, keyed to a fiscal year (from close to close) |
| `ledger reconstruct <tx-id>` | Print the `ledger add` command that would recreate a transaction |
| `ledger reconstruct --all` | Print every transaction as a `ledger add` command, in posted order (pipable to `sh` to rebuild the database) |
| `ledger tui` | Launch the TUI (default if no subcommand) |
| `ledger --help` | Full help |

### Entry format

`--entry ID[:CLASS]:AMOUNT` is the basic unit. Examples:

- `checking:asset:1000` — register `checking` as an asset (if new), record +$1000
- `income:income:-1000` — register `income` as income, record −$1000
- `checking:1000` — same as above if `checking` is already registered

Two entries minimum per transaction, and the amounts must sum to zero. The model layer rejects anything else at write time.

### Flags

- `--db <path>` / `LEDGER_DB=<path>` — use a specific database file (overrides everything)
- `--book <name>` / `--ledger <name>` / `LEDGER_BOOK=<name>` — name the current "set of books". Default DB is `ledger.db`; with `--book personal` it becomes `ledger.personal.db`. Multiple books coexist in the data dir.
- `--date YYYY-MM-DD` / `--memo "..."` — add only, both optional (date defaults to today, memo defaults to empty)

### TUI keys

Press `?` in the TUI to open a dialog listing every shortcut, grouped by what they do. The status line at the bottom of the TUI is intentionally terse; the dialog is the reference.

## Reconstructing transactions

For a single transaction, the TUI's `y` key copies the `ledger add` command that would recreate the selected row to your clipboard. On the CLI, `ledger reconstruct <TX_ID>` prints the same thing to stdout.

For the whole database, `ledger reconstruct --all` writes a series of `ledger add` commands in posted order, separated by blank lines. The output is a portable shell script — paste it into a file, edit what you need to fix, and replay it. This is the cleanest way to rewrite history when a transaction was entered with wrong values, since the alternative is to post a correction that confuses later reports.

```bash
# 1. Dump the current database to a script
ledger reconstruct --all > rebuild.sh

# 2. Edit rebuild.sh to fix the wrong values

# 3. Validate the script against a scratch database first
LEDGER_DB=/tmp/scratch.db sh rebuild.sh

# 4. If the audit looks right, replay against the real database
sh rebuild.sh
```

A few things to know:

- **DB path.** `reconstruct --all` reads from the current DB; the `ledger add` calls in the script use the current DB too. To write somewhere fresh, set `LEDGER_DB` in the replay step (e.g. `LEDGER_DB=/tmp/scratch.db sh rebuild.sh`). Otherwise you'll append to the original and get duplicates of everything.
- **New transaction IDs.** The rebuilt transactions get fresh ULIDs — they're not byte-identical to the originals. The financial content (dates, accounts, amounts, memos) is preserved.
- **Errors don't stop the script.** If your edit produces an unbalanced entry, `sh` reports the error and keeps going. Always validate against a scratch DB before applying to the real one. `ledger audit` over the rebuilt DB is a quick way to confirm the numbers match.

## Fiscal years

Income and expense accounts are period accounts: they accumulate during the year and are zeroed at year end. `ledger close` posts a single ordinary transaction that zeroes every income and expense account with a nonzero balance and rolls the net into `equity/net` (retained earnings):

```bash
ledger close --date 2025-12-31 --memo "FY2025 close"
```

A profitable year credits `equity/net` (a negative amount, same sign convention as income); a loss debits it. Assets, liabilities, and other equity accounts carry forward untouched.

Closing transactions are detected structurally by the reporting layer — a transaction counts as a close iff it touches income/expense accounts plus only equity accounts, and it leaves every income and expense account at a zero all-time balance. Nothing is marked in the schema, so closes survive `reconstruct --all` rebuilds.

### Starting a database mid-history

`ledger open` records opening balances on an empty database. You list the asset and liability balances you know; the command plugs the difference into `equity/net` so the transaction balances (that plug *is* the retained earnings of all prior years):

```bash
ledger open --date 2025-01-01 \
  --entry checking:asset:1000 \
  --entry mortgage:liability:-300
# equity/net:equity:-700 is added automatically
```

### Fiscal-year reports

`ledger audit --fy YEAR` reports on the fiscal year that ends with the close dated in YEAR: the period runs from the previous close (or the first transaction, for the first year) through the close date. `--fy latest` uses the most recent close, and `--json` works as with `--from/--to`:

```bash
ledger audit --fy 2025
ledger audit --fy latest --json
```

If two closes fall in the same calendar year, `--fy` refuses to guess; use `--from`/`--to` for an exact range.

## The accounting model

This is a **double-entry** ledger. Every transaction is a transfer between at least two accounts; nothing ever appears from nothing, and nothing vanishes.

### Accounts

Five account classes:

- **Asset** — things you own with value: `checking`, `savings`, `investments/retirement`, `receivable/alex`
- **Liability** — things you owe: `credit_card`, `mortgage`
- **Equity** — starting net worth and owner contributions: `opening balance`
- **Income** — inflows that increase net worth: `income/salary`, `income/interest`
- **Expense** — outflows that reduce net worth: `expenses/food`, `expenses/rent`

Account ids are normalized: lowercase, ASCII alphanumeric, with `/` as a hierarchy separator (`expenses/food` and `expenses/dining` group under `expenses`). Colons and dots are reserved.

### The two-side rule

Every transaction is a `Transaction` with a `Vec<Entry>`, where each entry has an account and a **signed** `Decimal` amount. The model layer enforces:

1. At least two entries (no single-side "transactions")
2. The sum of all amounts in a transaction is exactly zero (it balances)

Trying to post anything else returns an error before the DB is touched. This is the core invariant — it's what makes double-entry a useful discipline rather than a bookkeeping chore.

### What zero-sum means in practice

A paycheck `+1000 / -1000` is "checking went up by 1000, income went up by 1000" — but `income` is the contra-side, so its balance reads negative. The identity is:

```
Σ Assets  −  Σ Liabilities  −  Σ Equity  =  Σ Income  −  Σ Expenses
```

(equivalently: net worth = realized gains − realized losses). The TUI shows you the left side directly in the accounts view. As long as every transaction balances, the right side will always agree with the left, which is how you catch typos and missing entries.

### Examples

A 2-entry paycheck (most common):

```
checking:asset:1000
income:income:-1000
```

A split bill (3 entries):

```
expenses/dining:expense:80
checking:asset:-60
receivable/alex:asset:-20
```

A multi-leg paycheck with deductions (4 entries):

```
checking:asset:700
401k:asset:200
tax/withholding:liability:100
income:income:-1000
```

### What the model doesn't enforce

- **Class correctness.** You can post a debit to an `equity` account or a credit to an `expense` account. The model doesn't care. For a personal ledger, the class is a labeling tool, not a hard constraint.
- **Normal balance.** Asset accounts "should" have positive balances; liability accounts "should" have negative. We don't check. You can still write useful reports by summing the class as a whole.
- **Non-negative amounts on assets.** You can take a checking account below zero; the model will accept it. The bank statement is what tells you whether that happened in real life.

If you want stricter rules later, add a `validate_class(&Transaction)` step in `Transaction::new` and an `AccountClass::normal_sign()` method.

## Storage

Local SQLite file. Schema in `src/storage.rs`:

```
accounts(id, class)
transactions(id, posted_at, memo)
entries(transaction_id, account, amount)
```

Foreign keys are enabled per connection. Amounts are stored as text and aggregated in Rust (not in SQL) to preserve `Decimal` precision — SQLite's `SUM` would coerce to `REAL` and we'd lose the exact-decimal guarantee.

Default location: `~/Library/Application Support/ledger/ledger.db` on macOS, `~/.local/share/ledger/ledger.db` on Linux, `%APPDATA%/ledger/ledger.db` on Windows (via the `dirs` crate).

## Project structure

```
src/
├── main.rs       thin dispatcher → ledger::cli::execute
├── lib.rs        pub mod {cli, model, storage, tui}
├── model.rs      domain types, normalization, validation
├── storage.rs    rusqlite + schema
├── cli.rs        clap, add/income subcommands, db path resolution
└── tui.rs        ratatui, accounts + register views, filters
```

Domain types live in `model.rs` and are storage-agnostic. The storage layer in `storage.rs` is the only place that knows about SQLite. `cli.rs` and `tui.rs` are interfaces over the model + storage.

## What it doesn't do (yet)

- Edit or delete transactions
- Multi-currency
- Recurring transactions
- Import from CSV / OFX / bank statements
- Reports beyond "balance per account" (no P&L, no balance sheet, no cash flow)
- Multi-user, multi-device sync (single-user, single-system by design)
