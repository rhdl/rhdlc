#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::env;
use std::process;

mod resolve;
mod error;


fn main() {
    env_logger::init();

    let filepath = match env::args().skip(1).next() {
        Some(filename) => PathBuf::from(filename),
        _ => {
            eprintln!("Usage: dump-syntax path/to/filename.rs");
            process::exit(1);
        }
    };

    let tree = resolve::resolve_source_tree(&filepath);
    if let Err(errs) = tree {
        errs.iter().for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }
}
