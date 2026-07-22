#![allow(clippy::implicit_hasher)]
// `implicit_hasher` is a pedantic lint aimed at library authors: it suggests
// generic-izing HashMap parameters over a BuildHasher so callers aren't
// locked into RandomState. The ledger binary is the only consumer of this
// crate's types, so the API flexibility it buys isn't worth the boilerplate.

pub mod cli;
pub mod model;
pub mod storage;
pub mod tui;
