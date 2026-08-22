# AGENTS.md

Notes for AI agents working on the `ledger` codebase. If you're an agent reading this, the human already knows what the project is — they've been building it. Your job is to not break things they care about and to extend carefully.

## Build / lint / test

```bash
cargo build                # compile
cargo run -- --help        # smoke test the CLI
cargo fmt                  # required before commit
cargo fmt --check          # CI check
cargo clippy --all-targets -- -W clippy::pedantic
                           # the project keeps pedantic clippy clean
cargo test                 # there are no tests yet; add some if you change behavior
```

There is no `cargo test` suite. Tests should be added as you go — don't introduce regressions and not write a test for the change.

## Project layout

```
src/
├── main.rs       thin entrypoint → ledger::cli::execute
├── lib.rs        pub mod {cli, model, storage, tui}
├── model.rs      domain types, validation, AccountId normalization
├── storage.rs    rusqlite impl, schema in const SCHEMA
├── cli.rs        clap, subcommands, db path resolution
└── tui.rs        ratatui app, event loop, render
```

The library is a lib + bin in one crate. The binary is a 5-line `main.rs` that calls `cli::execute()`. This is intentional — keeps things testable, makes it trivial to add a second binary later (e.g., a long-running daemon).

## Layering

Three layers, strictly separated:

1. **`model.rs`** — domain types and invariants. Knows nothing about SQLite, clap, or ratatui. `Transaction::new` enforces the zero-sum invariant; `AccountId::parse` enforces normalization.
2. **`storage.rs`** — persistence. The only file that knows about SQLite. `pub(crate) Transaction::from_raw` exists so storage reads can skip re-validation.
3. **`cli.rs` / `tui.rs`** — interfaces. Parse args or events, call storage/model, format output.

If you find yourself importing `rusqlite` from `cli.rs` or `ratatui` from `storage.rs`, you're putting something in the wrong layer. The only cross-layer state is `Decimal` (used everywhere for money).

## Conventions

- **Money is `rust_decimal::Decimal`. Never `f64`.** The model uses it for exact arithmetic; storage stores it as TEXT and aggregates in Rust (because SQLite `SUM` would coerce to REAL and lose precision).
- **IDs are newtypes.** `AccountId(String)` is private-constructed (only `AccountId::parse`), `TransactionId(Ulid)` is copy. New id types should follow the same pattern: newtype + private field + `parse` constructor.
- **Errors use `thiserror::Error` derive.** Each module owns its error enum. Conversion happens via `From` impls at the boundary.
- **No comments unless asked.** Doc comments on public items are fine and clippy pedantic will ask for them on `Result`-returning functions. Inline narrative comments get removed.
- **Pedantic clippy is the bar.** `cargo clippy --all-targets -- -W clippy::pedantic` should pass clean. If you can't satisfy a lint, add a `#[allow(...)]` with a one-line explanation at the most local scope possible.
- **Account ids normalize to ASCII alphanumeric + `/`.** `is_ascii_alphanumeric()` is the predicate in `model.rs`. Colons and dots are reserved (don't allow them in account names). The CLI entry format is `id:class:amount`, which is why `:` is reserved.
- **Roll-ups belong in `model.rs`.** `Summary::from_balances` is the one place that turns per-account balances into book-level totals; it flips income to a positive magnitude and leaves liabilities negative so each reported column sums on its own. If you need another aggregate, put it there rather than in a render function — the identity `net_worth() + equity == activity.net()` is testable without a terminal.
- **A transaction's role is recorded, not inferred.** `TransactionKind` is `Normal` (the default) or one of two period boundaries, `Close` and `Open`. `Transaction::new` always produces `Normal`; `with_kind` is the only way to get a boundary and it validates the kind against the entries' account classes, so a stored marker can never describe a transaction that isn't shaped like one. If you need to know whether something is a close, read `tx.kind` — never re-derive it from the entry structure.
- **Amounts are signed.** Positive means "the account went up by this much", negative means "down". A `+1000` entry on `checking` and a `-1000` entry on `income` are the two sides of a paycheck.

## DB schema (storage.rs)

```
accounts(id TEXT PK, class TEXT CHECK in 'asset'/'liability'/'equity'/'income'/'expense')
transactions(id TEXT PK, posted_at TEXT, memo TEXT,
             kind TEXT NOT NULL DEFAULT 'normal' CHECK in 'normal'/'close'/'open')
entries(transaction_id TEXT FK→transactions, account TEXT FK→accounts, amount TEXT)
```

`kind` follows `accounts.class`: a text enum with a CHECK, not an integer code, so the database stays readable with a bare `sqlite3` query. There are no NULLs — `'normal'` is the default and the zero value. `Storage::add_transaction_kind` adds the column to databases written before it existed (`CREATE TABLE IF NOT EXISTS` will not), stamping every existing row `'normal'`.

`PRAGMA foreign_keys = ON` is set on every connection. Amounts are stored as text because `Decimal` is exact; do not change this to INTEGER or REAL. If you find yourself wanting to use `SUM()` in SQL, don't — iterate in Rust.

`list_transactions` does a `JOIN` and groups rows into `Vec<Transaction>` in Rust code (rows are ordered by `posted_at, id, e.rowid` so consecutive entries for the same transaction cluster together).

## TUI

`ratatui` + `crossterm`. The app loads all data into memory on startup; for a personal ledger this is fine. If you add pagination or lazy loading, the natural seam is `App::load()`.

The render path is: `ui()` builds a `Table` widget from filtered data, hands it `&mut app.table_state` for selection. Column widths are `Constraint::Length(N)`; right-aligned columns use `Text::from(...).right_aligned()`.

**Theme:** colors come from the `ACCENT` / `POSITIVE` / `NEGATIVE` consts at the top of `tui.rs`, always through the `accent()`, `heading()`, `dim()`, and `money_style()` helpers — don't hand-roll a `Style` with a literal color. They are ANSI-indexed (`Color::Cyan`, not `Color::Rgb`) so the user's terminal theme still picks the actual hue; keep it that way. Accent is chrome only (titles, table headers, cursor, modal borders, active filters). Money is colored by the *literal* sign of the amount, so an income account reads red — that is the signed convention showing through, not a bug; don't "fix" it by coloring per account class.

The selection cursor is an accent bar (`cursor_symbol()`) plus a bold row, deliberately *not* `Modifier::REVERSED`. `Table::render_rows` patches `row_highlight_style` over the cells after they render, so REVERSED would swap the money column's green/red foreground into a background and paint the selected row in blocks of color.

The summary's activity column toggles between two periods with `s` (`SummaryPeriod`, defaulting to `FiscalYear`). A fiscal year is "since open, or since the last closing transaction" — which is exactly what the nominal balances already hold, since a close zeroes them, so `Summary::from_balances` answers it for free. All-time has to be walked out of the transactions by `model::lifetime_activity`, skipping the closes themselves. Only the activity column changes: net worth is a position, not a flow, and must read the same in both modes.

The accounts view reserves its bottom 5 rows for the summary panel (`render_summary`), which totals `model::Summary` across **every** account — it deliberately ignores the search filter, because net worth over an arbitrary subset is meaningless; the block title says "all accounts" while a filter is active. `split_body` drops the panel entirely when the body is under `SUMMARY_MIN_BODY` rows so a short terminal keeps usable table rows, and the register view never shows it. The "activity since" label comes from `model::period_start`, computed once in `App::load()` rather than per frame.

The terminal is restored in a guard pattern inside `tui::run()` — the closure captures the terminal so `restore_terminal` runs even if `App::new` or the event loop errors.

**Modifier-key shortcut rule:** anything that modifies data takes Shift (i.e. an uppercase letter). Read-only navigation and filters use lowercase. So `D` deletes the selected transaction (with a y/n confirmation), and `C` opens the edit-accounts modal (also y/n to apply). `a` (add transaction) is the one deliberate exception — lowercase because it's the most frequent action; it still requires the mandatory y/n confirmation. Modals are centered overlays — see `centered_rect` and `render_edit_accounts_modal`. A confirmation step is mandatory for any destructive or mutating action from the TUI.

## CLI

`clap` with `derive` + `env` + `wrap_help` features. Subcommands are an `Option<Command>` so `ledger` with no args defaults to `Tui`. Don't add a subcommand without considering whether it should be the default.

Env vars: `LEDGER_DB` (full path override), `LEDGER_BOOK` (filename suffix, validated to be `is_ascii_alphanumeric()` and non-empty).

The `income` subcommand is a shortcut that builds two specific entries (`checking:asset:N`, `income:income:-N`) and routes them through the same `execute_transaction` helper as `add`. If you add more shortcuts, follow the same pattern — no parallel codepaths. `close` and `open` are shortcuts in the same vein: `close` zeroes every nonzero income/expense balance into `equity/net`; `open` plugs the balancing difference of known opening balances into `equity/net` and refuses a non-empty db. Both set a `TransactionKind` on the transaction they post, so a close is a close because it says so, not because its shape happens to look like one.

**Any marker on a transaction must round-trip through `ledger add`, or `reconstruct --all` silently drops it.** That is the whole constraint: rebuilds re-create every transaction through `add`, so `add` carries a `--kind` flag and `render_add_command` emits it for non-`Normal` transactions. If you add another per-transaction attribute, it needs the same treatment — a column alone is not enough, and the failure is silent. `model::closes` (Close only) feeds `audit --fy YEAR|latest`; `model::period_start` (Close **or** Open, whichever is latest) feeds the TUI summary, because both kinds start a fiscal period.

## Things to be careful about

- **Don't `cargo run` against the real data dir when testing.** Use `--db /tmp/test.db` or `LEDGER_DB=/tmp/test.db`. The TUI and CLI both default to `~/Library/Application Support/ledger/ledger.db` (macOS) / `~/.local/share/ledger/ledger.db` (Linux) / `%APPDATA%/ledger/ledger.db` (Windows), and there is no undo.
- **Don't change the schema without a migration.** Users have real data in the file. If you must add a column, add it to the `CREATE TABLE` and accept `NULL` for existing rows; don't `DROP` and `CREATE`.
- **Date parsing is `YYYY-MM-DD` only.** The CLI rejects anything else. If you need sub-day precision, change the parsing in `cli::parse_date` and the format in `storage::from_db` together — they're tied.
- **`TuiArgs` is currently empty.** Don't add fields to it without thinking about whether they should be `Cl`-level globals (like `--db` and `--book` already are) instead.

## When you're stuck

- Read the relevant module's `impl` block first — `model.rs::Transaction` and `storage.rs::Storage` carry the most weight and the most comments-via-naming.
- The accounting identity (`Σ Assets − Σ Liabilities − Σ Equity = Σ Income − Σ Expenses`) is a good test oracle. If you can write a transaction that breaks it, you've found a bug.
- The user reads diffs carefully. Small focused commits are appreciated. Don't refactor unrelated code in the same change.
