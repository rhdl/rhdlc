use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use petgraph::{
    graph::{DefaultIx, NodeIndex},
    Graph,
};
use syn::ItemMod;

use crate::error::{
    DirectoryError, DuplicateError, NotFoundError, PreciseSynParseError, ResolveError,
    UnexpectedModError,
};

/// Resolves source code for modules from their files recursively
/// Errors are related to file-reading issues, missing content, or duplicate files
/// Does not care about naming conflicts, as those are delegated to `ScopeBuilder`.
#[derive(Default)]
pub struct Resolver {
    cwd: PathBuf,
    pub file_graph: Graph<syn::File, syn::ItemMod>,
    pub errors: Vec<ResolveError>,
    ancestry: Vec<NodeIndex<DefaultIx>>,
}

impl Resolver {
    /// List of paths to resolve
    /// A top level entry point + crate entry points `lib.rs`
    pub fn resolve_forest(&mut self, paths: &Vec<&Path>) {
        paths
            .iter()
            .map(|path| {
                if path.is_dir() {
                    Err(DirectoryError(path.to_path_buf()).into())
                } else if path
                    .file_name()
                    .map(|osstr| osstr == "mod.rhdl")
                    .unwrap_or(false)
                {
                    Err(UnexpectedModError(path.to_path_buf()).into())
                } else {
                    Ok(path)
                }
            })
            .for_each(|path| match path {
                Ok(path) => self.resolve_path(path),
                Err(err) => self.errors.push(err),
            });
    }

    fn resolve_path(&mut self, path: &Path) {
        if path.is_file() {
            match Self::resolve_file(path) {
                Ok(file) => {
                    let idx = self.file_graph.add_node(file);
                    self.ancestry.push(idx);
                    self.cwd = path.to_owned();
                    self.cwd.pop();

                    let mods: Vec<ItemMod> = self.file_graph[idx]
                        .items
                        .iter()
                        .filter_map(|item| match item {
                            syn::Item::Mod(m) => Some(m.clone()),
                            _ => None,
                        })
                        .collect();
                    for m in mods {
                        self.resolve_mod(m);
                    }
                    self.ancestry.pop();
                }
                Err(err) => self.errors.push(err),
            }
        } else {
            todo!("Could be a broken symlink or something");
        }
    }

    /// If the code is in a mod.rhdl file, there could be more modules that need to be recursively resolved.
    fn resolve_mod(&mut self, item_mod: ItemMod) {
        if let Some(content) = item_mod.content {
            for item in content.1 {
                match item {
                    syn::Item::Mod(m) => {
                        if let None = m.content {
                            // A mod in a file can have declared sub-mods in files in ./mod/sub-mod.rs
                            todo!(
                            "Current implementation can't support this, and it's a rare edge-case"
                        );
                            self.resolve_mod(m);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        let ident = &item_mod.ident;
        let mod_file_path = self.cwd.join(format!("{}.rhdl", ident));
        let mod_folder_file_path = self.cwd.join(format!("{}/mod.rhdl", ident));

        let path = match (mod_file_path.is_file(), mod_folder_file_path.is_file()) {
            (true, false) => mod_file_path,
            (false, true) => mod_folder_file_path,
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

        let file = Self::resolve_file(&path);
        match file {
            Err(err) => {
                self.errors.push(err);
                return;
            }
            Ok(file) => {
                let mods: Vec<ItemMod> = file
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
                for m in mods {
                    self.resolve_mod(m);
                }
                self.cwd.pop();
                self.ancestry.pop();
            }
        }
    }

    fn resolve_file(path: &Path) -> Result<syn::File, ResolveError> {
        let mut file = File::open(&path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let tree = syn::parse_file(&content).map_err(|err| PreciseSynParseError {
            cause: err,
            code: content,
            path: path.to_owned(),
        })?;
        Ok(tree)
    }
}