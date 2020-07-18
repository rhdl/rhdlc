#![forbid(unsafe_code)]

use petgraph::dot::Dot;

use std::env;
use std::path::PathBuf;
use std::process;

mod error;
mod resolve;
mod scope;

fn main() {
    env_logger::init();

    let filepath = match env::args().skip(1).next() {
        Some(filename) => PathBuf::from(filename),
        _ => {
            eprintln!("Usage: rhdlc path/to/filename.rs");
            process::exit(1);
        }
    };

    let mut resolver = resolve::Resolver::default();
    resolver.resolve_forest(&vec![&filepath]);
    if resolver.errors.len() > 0 {
        resolver.errors.iter().for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }

    let mut item_scoper = scope::ScopeBuilder::from(&resolver.file_graph);
    item_scoper.stage_one();
    println!("{}", Dot::new(&item_scoper.scope_graph));
}
