fn main() {
    if let Err(e) = ledger::cli::execute() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
