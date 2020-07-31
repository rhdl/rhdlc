#![forbid(unsafe_code)]

use clap::{clap_app, crate_authors, crate_description, crate_version};
use petgraph::dot::Dot;

use std::env;
use std::process;

mod error;
mod ident;
mod resolve;
mod scope;

fn main() {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "rhdlc=info")
    }
    env_logger::init();
    let matches = clap_app!(rhdlc =>
        (version: crate_version!())
        (author: crate_authors!())
        (about: crate_description!())
        (@arg FILE: "The top level RHDL file")
    )
    .get_matches();

    let mut resolver = resolve::Resolver::default();
    match matches.value_of("FILE") {
        Some("-") | None => {
            resolver.resolve_tree(resolve::ResolutionSource::Stdin);
        }
        Some(path) => {
            resolver.resolve_tree(resolve::ResolutionSource::File(path.into()));
        }
    }

    if !resolver.errors.is_empty() {
        resolver.errors.iter().for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }

    let mut scope_builder = scope::ScopeBuilder::from(&resolver.file_graph);
    scope_builder.build_graph();
    scope_builder.check_graph();
    if !scope_builder.errors.is_empty() {
        scope_builder
            .errors
            .iter()
            .for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }

    println!("{}", Dot::new(&scope_builder.scope_graph));
}
