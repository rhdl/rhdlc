#![forbid(unsafe_code)]

use clap::{clap_app, crate_authors, crate_description, crate_version};
use petgraph::dot::Dot;

use std::env;

mod error;
mod find_file;
mod resolution;

use find_file::{FileContentSource, FileFinder};
use resolution::Resolver;

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
        Some("-") | None => {
            FileContentSource::Reader("stdin".to_string(), Box::new(std::io::stdin()))
        }
        Some(path) => FileContentSource::File(path.into()),
    };

    let out = entry(src);
    eprint!("{}", out);
}

fn entry(src: FileContentSource) -> String {
    let mut acc = String::default();

    let mut finder = FileFinder::default();
    finder.find_tree(src);
    finder
        .errors
        .iter()
        .map(|err| format!("{}", err))
        .for_each(|err| acc += &err);

    let mut scope_builder = Resolver::from(&finder.file_graph);
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
    fn compile_fail_find_file() {
        fail_test_looper("./test/compile-fail/find-file")
    }

    #[test]
    fn compile_fail_resolution_use() {
        fail_test_looper("./test/compile-fail/resolution/use")
    }

    #[test]
    fn compile_fail_resolution_pub() {
        fail_test_looper("./test/compile-fail/resolution/pub")
    }

    #[test]
    fn compile_fail_resolution_redefinition() {
        fail_test_looper("./test/compile-fail/resolution/redefinition")
    }

    #[test]
    fn compile_fail_identifier() {
        fail_test_looper("./test/compile-fail/identifier")
    }

    #[test]
    fn compile_fail_parse() {
        fail_test_looper("./test/compile-fail/parse")
    }

    #[test]
    fn compile_fail_unsupported() {
        fail_test_looper("./test/compile-fail/unsupported")
    }

    #[test]
    fn compile_pass_resolution_use() {
        success_test_looper("./test/compile-pass/resolution/use")
    }

    #[test]
    fn compile_pass_stdin() {
        let output = super::entry(crate::find_file::FileContentSource::Reader(
            "string".to_string(),
            Box::new("struct a {}".as_bytes()),
        ));
        assert_eq!(output, "");
    }

    fn fail_test_looper(dir: &str) {
        use pretty_assertions::assert_eq;
        use std::fs;
        use std::io::Write;
        for test in fs::read_dir(dir).unwrap() {
            let test = test.unwrap();
            let input = test.path().join("top.rhdl");
            let expected = fs::read_to_string(test.path().join("expected.txt"))
                .expect(&test.path().join("expected.txt").to_string_lossy());
            let output = super::entry(crate::find_file::FileContentSource::File(input));
            eprintln!("{}", test.path().to_string_lossy());
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

    fn success_test_looper(dir: &str) {
        use pretty_assertions::assert_eq;
        use std::fs;
        use std::io::Write;
        for test in fs::read_dir(dir).unwrap() {
            let test = test.unwrap();
            let output = super::entry(crate::find_file::FileContentSource::File(test.path()));
            eprintln!("{}", test.path().to_string_lossy());
            std::io::stderr()
                .flush()
                .ok()
                .expect("Could not flush stderr");
            std::io::stdout()
                .flush()
                .ok()
                .expect("Could not flush stdout");
            assert_eq!(output, "");
        }
    }
}
