use crate::error::FileFindingError;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use codespan::{FileId, Files};
use codespan_reporting::diagnostic::Diagnostic;
use fxhash::FxHashMap as HashMap;
use rhdl::ast::{File as RhdlFile, Ident, Item, ItemMod, ModContent};
use rhdl::parser::FileParser;

use crate::error;

#[derive(Debug)]
pub struct File {
    provider: FileContentProvider,
    content: String,
    parsed: RhdlFile,
    parent: Option<FileId>,
}

impl AsRef<str> for File {
    fn as_ref(&self) -> &str {
        &self.content
    }
}

#[derive(Default, Debug)]
pub struct FileGraph {
    pub inner: Files<File>,
    indices: Vec<FileId>,
    pub roots: Vec<FileId>,
    pub children: HashMap<FileId, Vec<(Vec<Ident>, FileId)>>,
}

impl std::ops::Index<FileId> for FileGraph {
    type Output = File;
    fn index(&self, id: FileId) -> &<Self as std::ops::Index<FileId>>::Output {
        self.inner.source(id)
    }
}

impl FileGraph {
    fn add_node(&mut self, name: OsString, file: File, parent: Option<FileId>) -> FileId {
        let idx = self.inner.add(name, file);
        if parent.is_none() {
            self.roots.push(idx);
        }
        self.indices.push(idx);
        idx
    }

    fn add_edge(&mut self, parent: FileId, idents: Vec<Ident>, child: FileId) {
        self.children
            .entry(parent)
            .or_default()
            .push((idents, child));
    }

    pub fn iter(&self) -> impl Iterator<Item = &FileId> {
        self.indices.iter()
    }
}

const STDIN_FALLBACK_EXTENSION: &str = "rhdl";

/// Finds source code for modules from their files recursively
/// Errors are related to file-reading issues, missing content, or conflicting files
/// Does not care about naming conflicts, as those are handled downstream.
#[derive(Default)]
pub struct FileFinder {
    pub file_graph: FileGraph,
    pub errors: Vec<Diagnostic<FileId>>,
    cwd: PathBuf,
    extension: String,
    ancestry: Vec<FileId>,
    ident_path: Vec<Ident>,
}

pub enum FileContentProvider {
    File(PathBuf),
    Reader(String, Box<dyn Read>),
}

impl FileContentProvider {
    fn name(&self) -> OsString {
        match self {
            Self::File(path) => path.as_os_str().to_os_string(),
            Self::Reader(name, _) => name.clone().into(),
        }
    }
}

impl std::fmt::Debug for FileContentProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use FileContentProvider::*;
        match self {
            File(path) => f.debug_tuple("FileContentProvider").field(&path).finish(),
            Reader(name, _) => f.debug_tuple("FileContentProvider").field(&name).finish(),
        }
    }
}

impl FileFinder {
    /// A top level entry point
    /// TODO: handle a top level file named `a.rhdl` with `mod a;` declared.
    pub fn find_tree(&mut self, root_provider: FileContentProvider) {
        let root_name = root_provider.name();
        let root_path = match &root_provider {
            FileContentProvider::File(path) => Some(path.clone()),
            _ => None,
        };
        let root_file = match Self::find(root_provider, None) {
            Ok(root_file) => root_file,
            Err(err) => {
                self.errors.push(err.diagnostic(root_name, None));
                return;
            }
        };
        let root_file_id = self.file_graph.add_node(root_name, root_file.into(), None);
        let mods: Vec<ItemMod> = self.file_graph[root_file_id]
            .parsed
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mod(m) => Some(m),
                _ => None,
            })
            .cloned()
            .collect();

        self.cwd = if let Some(cwd) = root_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_owned)
        {
            cwd
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd,
                Err(cause) => {
                    self.errors.push(error::working_directory(cause));
                    return;
                }
            }
        };

        self.extension = root_path
            .and_then(|p| {
                p.extension()
                    .map(OsStr::to_string_lossy)
                    .map(|cow| cow.to_string())
            })
            .unwrap_or_else(|| STDIN_FALLBACK_EXTENSION.to_owned());

        self.ancestry.push(root_file_id);
        for child in mods {
            if child.content.is_file() {
                self.find_mod(&child);
            } else {
                self.find_mod_with_content(&child);
            }
        }
        self.ancestry.pop();
    }

    /// If the code is in a mod file, there could be more modules that need to be recursively found.
    fn find_mod(&mut self, item_mod: &ItemMod) {
        self.ident_path.push(item_mod.ident.clone());
        let mut mod_file_path = self.cwd.clone();
        self.ident_path.iter().for_each(|ident| {
            let ident = ident.to_string();
            mod_file_path.push(ident.strip_prefix("r#").unwrap_or(&ident));
        });
        let mod_file_path = mod_file_path.with_extension(&self.extension);
        let mod_folder_file_path = mod_file_path.join("mod").with_extension(&self.extension);

        let (found_file_path, found_file) = match (
            Self::find(
                FileContentProvider::File(mod_file_path.clone()),
                self.ancestry.last().cloned().map(|id| (id, item_mod)),
            ),
            Self::find(
                FileContentProvider::File(mod_folder_file_path.clone()),
                self.ancestry.last().cloned().map(|id| (id, item_mod)),
            ),
        ) {
            (Ok(found_file), Err(_)) => (mod_file_path, found_file),
            (Err(_), Ok(found_file)) => (mod_folder_file_path, found_file),
            (Ok(found_file), Ok(_found_mod_file)) => {
                self.errors.push(error::conflicting_mod_files(
                    self.ancestry.last().cloned(),
                    &item_mod,
                    &mod_file_path,
                    &mod_folder_file_path,
                ));
                // Create an error, but assume name.rhdl is the correct one and keep going
                (mod_file_path, found_file)
            }
            (Err(err1), Err(err2)) => {
                if err1.is_io_not_found() && err2.is_io_not_found() {
                    // Only display a single not found error if both are not found
                    self.errors.push(err1.diagnostic(
                        mod_file_path.into_os_string(),
                        self.ancestry.last().cloned().map(|id| (id, item_mod)),
                    ));
                } else {
                    // Ignore a not found error since we know at least 1 was found
                    // Create a conflict error if both were found
                    if !err1.is_io_not_found() && !err2.is_io_not_found() {
                        self.errors.push(error::conflicting_mod_files(
                            self.ancestry.last().cloned(),
                            &item_mod,
                            &mod_file_path,
                            &mod_folder_file_path,
                        ));
                    }
                    if !err1.is_io_not_found() {
                        self.errors.push(err1.diagnostic(
                            mod_file_path.into_os_string(),
                            self.ancestry.last().cloned().map(|id| (id, item_mod)),
                        ));
                    }
                    if !err2.is_io_not_found() {
                        self.errors.push(err2.diagnostic(
                            mod_folder_file_path.into_os_string(),
                            self.ancestry.last().cloned().map(|id| (id, item_mod)),
                        ));
                    }
                }
                return;
            }
        };

        let mods: Vec<ItemMod> = found_file
            .parsed
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mod(m) => Some(m),
                _ => None,
            })
            .cloned()
            .collect();
        let idx = self.file_graph.add_node(
            found_file_path.into_os_string(),
            found_file.into(),
            self.ancestry.last().cloned(),
        );
        if let Some(parent) = self.ancestry.last().cloned() {
            self.file_graph
                .add_edge(parent, self.ident_path.clone(), idx);
        }

        self.ancestry.push(idx);
        for m in &mods {
            if m.content.is_file() {
                self.find_mod(m);
            } else {
                self.find_mod_with_content(m);
            }
        }
        self.ancestry.pop();
        self.ident_path.pop();
    }

    /// A mod in a file can have declared sub-mods in files in ./mod/sub-mod.rs
    fn find_mod_with_content(&mut self, item_mod: &ItemMod) {
        if let ModContent::Here(here) = &item_mod.content {
            self.ident_path.push(item_mod.ident.clone());
            for item in &here.items {
                if let Item::Mod(m) = item {
                    if m.content.is_file() {
                        self.find_mod(m);
                    } else {
                        self.find_mod_with_content(m);
                    }
                }
            }
            self.ident_path.pop();
        }
    }

    fn find(
        mut provider: FileContentProvider,
        parent: Option<(FileId, &ItemMod)>,
    ) -> Result<File, FileFindingError> {
        let content = match &mut provider {
            FileContentProvider::File(path) => fs::File::open(&path).and_then(|mut f| {
                let mut content = String::new();
                f.read_to_string(&mut content)?;
                Ok(content)
            }),
            FileContentProvider::Reader(_, reader) => {
                let mut content = String::new();
                reader.read_to_string(&mut content).map(|_| content)
            }
        };
        match content {
            Ok(content) => match FileParser::new().parse(&content) {
                Err(err) => Err(FileFindingError::Parse(error::parse(
                    provider.name(),
                    parent,
                    err,
                ))),
                Ok(parsed) => Ok(File {
                    provider,
                    content,
                    parsed,
                    parent: parent.map(|(id, _)| id),
                }),
            },
            Err(err) => Err(FileFindingError::Io(err)),
        }
    }
}
