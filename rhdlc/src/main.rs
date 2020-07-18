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

    let forest = resolve::Resolver::resolve_forest(&vec![&filepath]);
    if let Err(errs) = forest {
        eprintln!("{:?}", errs);
        errs.iter().for_each(|err| eprintln!("{}", err));
        process::exit(1)
    }
    let forest = forest.unwrap();

    let mut item_scoper = scope::ScopeBuilder::default();
    // item_scoper.stage_one(&tree);
    // println!(
    //     "{}",
    //     Dot::new(&item_scoper.graph)
    // );
}
