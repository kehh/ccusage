use ccusage_adapter_common::read_files_parallel;
use ccusage_core::*;

mod loader;
mod parser;
mod paths;

pub use loader::load_entries;
