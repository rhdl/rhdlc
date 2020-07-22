use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use petgraph::{graph::NodeIndex, Graph};
use syn::{Ident, Item, ItemMod};

use crate::error::{
    DirectoryError, DuplicateError, NotFoundError, PreciseSynParseError, ResolveError,
    WrappedIoError,
};

#[derive(Debug)]
pub struct File {
    pub content: String,
    pub syn: syn::File,
    pub path: PathBuf,
}

pub type FileGraph = Graph<File, Vec<Ident>>;

/// Resolves source code for modules from their files recursively
/// Errors are related to file-reading issues, missing content, or duplicate files
/// Does not care about naming conflicts, as those are delegated to `ScopeBuilder`.
#[derive(Default)]
pub struct Resolver {
    cwd: PathBuf,
    pub file_graph: FileGraph,
    pub errors: Vec<ResolveError>,
    ancestry: Vec<NodeIndex>,
    extension: String,
}

impl Resolver {
    /// A top level entry point
    pub fn resolve(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.errors.push(DirectoryError(path.to_path_buf()).into());
        } else {
            match Self::resolve_file(path.clone()) {
                Ok(file) => {
                    let idx = self.file_graph.add_node(file.into());
                    self.ancestry.push(idx);
                    self.cwd = path.parent().unwrap().to_owned();
                    let mods: Vec<ItemMod> = self.file_graph[idx]
                        .syn
                        .items
                        .iter()
                        .filter_map(|item| match item {
                            Item::Mod(m) => Some(m),
                            _ => None,
                        })
                        .map(|m| m.clone())
                        .collect();
                    self.extension = path
                        .extension()
                        .map(OsStr::to_string_lossy)
                        .unwrap_or_default()
                        .to_string();
                    for m in mods {
                        if let None = m.content {
                            self.resolve_mod(m, vec![]);
                        } else {
                            self.resolve_mod_with_content(m, vec![]);
                        }
                    }
                    self.ancestry.pop();
                }
                Err(err) => self.errors.push(err),
            }
        }
    }

    /// If the code is in a mod file, there could be more modules that need to be recursively resolved.
    fn resolve_mod(&mut self, item_mod: ItemMod, mut ident_path: Vec<Ident>) {
        ident_path.push(item_mod.ident.clone());
        let mut mod_file_path = self.cwd.clone();
        ident_path
            .iter()
            .for_each(|ident| mod_file_path.push(ident.to_string()));
        let mod_folder_file_path = mod_file_path
            .clone()
            .join("mod")
            .with_extension(&self.extension);
        let mod_file_path = mod_file_path.with_extension(&self.extension);

        let (path, is_folder) = match (mod_file_path.is_file(), mod_folder_file_path.is_file()) {
            (true, false) => (mod_file_path, false),
            (false, true) => (mod_folder_file_path, true),
            (true, true) => {
                self.errors.push(
                    DuplicateError {
                        ident_path,
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
                        ident_path,
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
                        Item::Mod(m) => Some(m),
                        _ => None,
                    })
                    .map(|m| m.clone())
                    .collect();
                let idx = self.file_graph.add_node(file.into());
                if let Some(parent) = self.ancestry.last() {
                    // Ok to use the clone because it'll just be `mod abc;`
                    self.file_graph.add_edge(*parent, idx, vec![item_mod.ident]);
                }

                self.ancestry.push(idx);
                for m in mods {
                    if let None = m.content {
                        self.resolve_mod(m, ident_path.clone());
                    } else {
                        self.resolve_mod_with_content(m, ident_path.clone());
                    }
                }
                self.ancestry.pop();
            }
        }
    }

    fn resolve_mod_with_content(&mut self, item_mod: ItemMod, mut ident_path: Vec<Ident>) {
        if let Some(content) = item_mod.content {
            ident_path.push(item_mod.ident);
            for item in content.1 {
                match item {
                    Item::Mod(m) => {
                        if let None = m.content {
                            // A mod in a file can have declared sub-mods in files in ./mod/sub-mod.rs
                            self.resolve_mod(m, ident_path.clone());
                        } else {
                            self.resolve_mod_with_content(m, ident_path.clone());
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
    }

    fn resolve_file(path: PathBuf) -> Result<File, ResolveError> {
        let mut file = fs::File::open(&path).map_err(|cause| WrappedIoError {
            path: path.clone(),
            cause,
        })?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|cause| WrappedIoError {
                path: path.clone(),
                cause,
            })?;
        match syn::parse_file(&content) {
            Err(err) => Err(PreciseSynParseError {
                cause: err,
                code: content,
                path: path,
            }
            .into()),
            Ok(syn) => Ok(File { path, content, syn }),
        }
    }
}
