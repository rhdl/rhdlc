#![forbid(unsafe_code)]

use petgraph::dot::Dot;

use std::env;
use std::process;

mod error;
mod resolve;
mod scope;

fn main() {
    env_logger::init();

    let arg = match env::args().skip(1).next() {
        Some(arg) => arg,
        _ => {
            eprintln!("Usage: rhdlc path/to/filename.rs");
            process::exit(1);
        }
    };

    let mut resolver = resolve::Resolver::default();
    match arg.as_str() {
        "-" => {
            resolver.resolve_tree(resolve::ResolutionSource::Stdin);
        }
        path => {
            resolver.resolve_tree(resolve::ResolutionSource::File(path.into()));
        }
    }

    if resolver.errors.len() > 0 {
        resolver.errors.iter().for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }

    let mut scope_builder = scope::ScopeBuilder::from(&resolver.file_graph);
    scope_builder.build_graph();
    scope_builder.check_graph();
    if scope_builder.errors.len() > 0 {
        scope_builder
            .errors
            .iter()
            .for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }

    println!("{}", Dot::new(&scope_builder.scope_graph));
}
