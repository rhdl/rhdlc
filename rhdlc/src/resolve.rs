use std::fs;
use std::io::Read;
use std::path::PathBuf;

use petgraph::{graph::NodeIndex, Graph};
use syn::ItemMod;

use crate::error::{
    DirectoryError, DuplicateError, NotFoundError, PreciseSynParseError, ResolveError,
    UnexpectedModError,
};

#[derive(Debug)]
pub struct File {
    pub content: String,
    pub syn: syn::File,
    pub path: PathBuf,
}

pub type FileGraph = Graph<File, ItemMod>;

/// Resolves source code for modules from their files recursively
/// Errors are related to file-reading issues, missing content, or duplicate files
/// Does not care about naming conflicts, as those are delegated to `ScopeBuilder`.
#[derive(Default)]
pub struct Resolver {
    cwd: PathBuf,
    pub file_graph: FileGraph,
    pub errors: Vec<ResolveError>,
    ancestry: Vec<NodeIndex>,
}

const RHDL_EXTENSION: &'static str = "rhdl";

impl Resolver {
    /// List of paths to resolve
    /// A top level entry point + crate entry points `lib.rs`
    pub fn resolve_forest(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if path.is_dir() {
                self.errors.push(DirectoryError(path.to_path_buf()).into());
            } else if path
                .file_stem()
                .map(|osstr| osstr == "mod")
                .unwrap_or(false)
            {
                self.errors
                    .push(UnexpectedModError(path.to_path_buf()).into());
            } else {
                self.resolve_path(path);
            }
        }
    }

    fn resolve_path(&mut self, path: PathBuf) {
        if path.is_file() {
            match Self::resolve_file(path.clone()) {
                Ok(file) => {
                    let idx = self.file_graph.add_node(file);
                    self.ancestry.push(idx);
                    self.cwd = path.parent().unwrap().to_owned();

                    let mods: Vec<ItemMod> = self.file_graph[idx]
                        .syn
                        .items
                        .iter()
                        .filter_map(|item| match item {
                            syn::Item::Mod(m) => Some(m.clone()),
                            _ => None,
                        })
                        .collect();
                    for m in mods {
                        self.resolve_mod(
                            m,
                            &path
                                .extension()
                                .map(|e| e.to_string_lossy())
                                .unwrap_or(RHDL_EXTENSION.into()),
                        );
                    }
                    self.ancestry.pop();
                }
                Err(err) => self.errors.push(err),
            }
        } else {
            todo!("Could be a broken symlink or file doesn't exist");
        }
    }

    /// If the code is in a mod file, there could be more modules that need to be recursively resolved.
    fn resolve_mod(&mut self, item_mod: ItemMod, extension: &str) {
        if let Some(content) = item_mod.content {
            for item in content.1 {
                match item {
                    syn::Item::Mod(m) => {
                        if let None = m.content {
                            // A mod in a file can have declared sub-mods in files in ./mod/sub-mod.rs
                            todo!(
                            "Current implementation can't support this, and it's a rare edge-case"
                        );
                            self.resolve_mod(m, extension);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        let ident = &item_mod.ident;
        dbg!(&self.cwd);
        let mod_file_path = self.cwd.join(format!("{}.{}", ident, extension));
        let mod_folder_file_path = self.cwd.join(format!("{}/mod.{}", ident, extension));
        dbg!(&mod_file_path, &mod_folder_file_path);

        let (path, is_folder) = match (mod_file_path.is_file(), mod_folder_file_path.is_file()) {
            (true, false) => (mod_file_path, false),
            (false, true) => (mod_folder_file_path, true),
            (true, true) => {
                self.errors.push(
                    DuplicateError {
                        ident: ident.clone(),
                        file: mod_file_path,
                        folder: mod_folder_file_path,
                    }
                    .into(),
                );
                return;
            }
            (false, false) => {
                self.errors.push(
                    NotFoundError {
                        file: mod_file_path,
                        folder: mod_folder_file_path,
                        ident: ident.clone(),
                    }
                    .into(),
                );
                return;
            }
        };

        let file = Self::resolve_file(path.clone());
        match file {
            Err(err) => {
                self.errors.push(err);
                return;
            }
            Ok(file) => {
                let mods: Vec<ItemMod> = file
                    .syn
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        syn::Item::Mod(m) => Some(m.clone()),
                        _ => None,
                    })
                    .collect();
                let idx = self.file_graph.add_node(file);
                if let Some(parent) = self.ancestry.last() {
                    // Ok to use the clone because it'll just be `mod abc;`
                    self.file_graph.add_edge(*parent, idx, item_mod);
                }

                self.ancestry.push(idx);
                self.cwd = path.parent().unwrap().to_owned();
                dbg!(&self.cwd);
                for m in mods {
                    self.resolve_mod(m, extension);
                }
                if is_folder {
                    self.cwd.pop();
                }
                self.ancestry.pop();
            }
        }
    }

    fn resolve_file(path: PathBuf) -> Result<File, ResolveError> {
        let mut file = fs::File::open(&path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        match syn::parse_file(&content) {
            Err(err) => Err(PreciseSynParseError {
                cause: err,
                code: content,
                path: path.to_owned(),
            }
            .into()),
            Ok(syn) => Ok(File { path, content, syn }),
        }
    }
}
