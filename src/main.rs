#![forbid(unsafe_code)]

use clap::{clap_app, crate_authors, crate_description, crate_version};
use petgraph::dot::Dot;

use std::env;

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
    let mut acc = String::default();

    let mut resolver = resolve::Resolver::default();
    resolver.resolve_tree(src);
    resolver
        .errors
        .iter()
        .map(|err| format!("{}", err))
        .for_each(|err| acc += &err);

    let mut scope_builder = scope::ScopeBuilder::from(&resolver.file_graph);
    scope_builder.build_graph();
    scope_builder.check_graph();
    scope_builder
        .errors
        .iter()
        .map(|err| format!("{}", err))
        .for_each(|err| acc += &err);

    #[cfg(not(test))]
    println!("{}", Dot::new(&scope_builder.scope_graph));

    acc
}

#[cfg(test)]
mod test {
    #[test]
    fn compile_fail_file_resolution() {
        test_looper("./test/compile-fail/file-resolution");
    }

    #[test]
    fn compile_fail_scope() {
        test_looper("./test/compile-fail/scope");
    }

    #[test]
    fn compile_fail_identifier() {
        test_looper("./test/compile-fail/identifier");
    }

    #[test]
    fn compile_fail_parse() {
        test_looper("./test/compile-fail/parse");
    }

    fn test_looper(dir: &str) {
        use pretty_assertions::assert_eq;
        use std::fs;
        use std::io::Write;
        for test in fs::read_dir(dir).unwrap() {
            let test = test.unwrap();
            let input = test.path().join("top.rhdl");
            dbg!(input.to_string_lossy());
            let expected = fs::read_to_string(test.path().join("expected.txt")).unwrap();
            let output = super::entry(crate::resolve::ResolutionSource::File(input));
            std::io::stderr()
                .flush()
                .ok()
                .expect("Could not flush stderr");
            std::io::stdout()
                .flush()
                .ok()
                .expect("Could not flush stdout");
            assert_eq!(output, expected);
        }
    }
}
