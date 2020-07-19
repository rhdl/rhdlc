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
    resolver.resolve_forest(vec![filepath]);
    if resolver.errors.len() > 0 {
        resolver.errors.iter().for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }

    let mut scope_builder = scope::ScopeBuilder::from(&resolver.file_graph);
    scope_builder.build_graph();
    // scope_builder.check_graph();
    println!("{}", Dot::new(&scope_builder.scope_graph));
}
