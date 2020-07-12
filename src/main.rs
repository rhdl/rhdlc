#![forbid(unsafe_code)]

use std::path::PathBuf;
use syn::visit_mut::VisitMut;
use std::env;
use std::fs::File;
use std::io::Read;
use std::process;

mod resolve;
mod error;

fn main() {
    env_logger::init();

    let filename = match env::args().skip(1).next() {
        Some(filename) => PathBuf::from(filename),
        _ => {
            eprintln!("Usage: dump-syntax path/to/filename.rs");
            process::exit(1);
        }
    };
    let mut file = File::open(&filename).unwrap();

    let mut src = String::new();
    file.read_to_string(&mut src).unwrap();

    let mut tree = syn::parse_file(&src).unwrap();
    let mut resolver = resolve::ModResolver::new(filename.parent().unwrap());
    resolver.visit_file_mut(&mut tree);
    if !resolver.errors.is_empty() {
        for err in resolver.errors {
            eprintln!("{}", err);
        }
    }
}
