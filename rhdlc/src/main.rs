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

    let src = match matches.value_of("FILE") {
        Some("-") | None => resolve::ResolutionSource::Stdin,
        Some(path) => resolve::ResolutionSource::File(path.into()),
    };

    let out = entry(src);
    eprint!("{}", out);
}

fn entry(src: resolve::ResolutionSource) -> String {
    let mut resolver = resolve::Resolver::default();
    resolver.resolve_tree(src);
    if !resolver.errors.is_empty() {
        return resolver
            .errors
            .iter()
            .map(|err| format!("{}", err))
            .collect();
    }

    let mut scope_builder = scope::ScopeBuilder::from(&resolver.file_graph);
    scope_builder.build_graph();
    scope_builder.check_graph();
    if !scope_builder.errors.is_empty() {
        return scope_builder
            .errors
            .iter()
            .map(|err| format!("{}", err))
            .collect();
    }

    #[cfg(not(test))]
    println!("{}", Dot::new(&scope_builder.scope_graph));

    String::default()
}

#[cfg(test)]
mod test {
    #[test]
    fn compile_fail() {
        use pretty_assertions::assert_eq;
        use std::fs;

        for test in fs::read_dir("./test/compile-fail").unwrap() {
            let test = test.unwrap();
            dbg!(test.path().to_string_lossy());

            let input = test.path().join("top.rhdl");
            let expected = fs::read_to_string(test.path().join("expected.txt")).unwrap();
            let output = super::entry(crate::resolve::ResolutionSource::File(input));
            assert_eq!(output, expected);
        }
    }
}
