use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use codespan::{FileId, Files};
use codespan_reporting::diagnostic::Diagnostic;
use fxhash::FxHashMap as HashMap;
use rhdl::ast::{File as RhdlFile, Ident, Item, ItemMod, ModContent, Spanned};
use rhdl::parser::FileParser;

use crate::error;

#[derive(Debug)]
pub struct File {
    src: FileContentSource,
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

    pub fn node_indices(&self) -> impl Iterator<Item = &FileId> {
        self.indices.iter()
    }
}

#[derive(Clone)]
struct SpanSource {
    parent_file_id: FileId,
    ident_path: Vec<Ident>,
}

const STDIN_FALLBACK_EXTENSION: &str = "rhdl";

/// Finds source code for modules from their files recursively
/// Errors are related to file-reading issues, missing content, or duplicate files
/// Does not care about naming conflicts, as those are delegated to `ScopeBuilder`.
#[derive(Default)]
pub struct FileFinder {
    pub file_graph: FileGraph,
    pub errors: Vec<Diagnostic<FileId>>,
    cwd: PathBuf,
    ancestry: Vec<FileId>,
    extension: String,
}

pub enum FileContentSource {
    File(PathBuf),
    Reader(String, Box<dyn Read>),
}

impl FileContentSource {
    fn name(&self) -> OsString {
        match self {
            Self::File(path) => path.as_os_str().to_os_string(),
            Self::Reader(name, _) => name.clone().into(),
        }
    }
}

impl std::fmt::Debug for FileContentSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use FileContentSource::*;
        match self {
            File(path) => f.debug_tuple("FileContentSource").field(&path).finish(),
            Reader(name, _) => f.debug_tuple("FileContentSource").field(&name).finish(),
        }
    }
}

impl FileFinder {
    /// A top level entry point
    /// TODO: handle a top level file named `a.rhdl` with `mod a;` declared.
    pub fn find_tree(&mut self, src: FileContentSource) {
        let name = src.name();
        let path = match &src {
            FileContentSource::File(path) => Some(path.clone()),
            _ => None,
        };
        match Self::find(src, None) {
            Ok(file) => {
                let idx =
                    self.file_graph
                        .add_node(name, file.into(), self.ancestry.last().cloned());
                self.ancestry.push(idx);
                let mods: Vec<ItemMod> = self.file_graph[idx]
                    .parsed
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        Item::Mod(m) => Some(m),
                        _ => None,
                    })
                    .cloned()
                    .collect();

                if let Some(cwd) = path
                    .as_ref()
                    .and_then(|p| p.parent().map(|parent| parent.to_owned()))
                {
                    self.cwd = cwd;
                } else {
                    match std::env::current_dir() {
                        Ok(cwd) => self.cwd = cwd,
                        Err(cause) => {
                            self.errors.push(error::working_directory(cause));
                            return;
                        }
                    }
                }

                self.extension = path
                    .and_then(|p| {
                        p.extension()
                            .map(OsStr::to_string_lossy)
                            .map(|cow| cow.to_string())
                    })
                    .unwrap_or_else(|| STDIN_FALLBACK_EXTENSION.to_owned());

                for m in mods {
                    if m.content.is_file() {
                        self.find_mod(
                            m,
                            SpanSource {
                                parent_file_id: idx,
                                ident_path: vec![],
                            },
                        );
                    } else {
                        self.find_mod_with_content(
                            m,
                            SpanSource {
                                parent_file_id: idx,
                                ident_path: vec![],
                            },
                        );
                    }
                }
                self.ancestry.pop();
            }
            Err(err) => self.errors.push(err),
        }
    }

    /// If the code is in a mod file, there could be more modules that need to be recursively found.
    fn find_mod(&mut self, item_mod: ItemMod, mut span: SpanSource) {
        span.ident_path.push(item_mod.ident.clone());
        let mut mod_file_path = self.cwd.clone();
        span.ident_path.iter().for_each(|ident| {
            let ident = ident.to_string();
            mod_file_path.push(ident.strip_prefix("r#").unwrap_or(&ident));
        });
        let mod_file_path = mod_file_path.with_extension(&self.extension);
        let mod_folder_file_path = mod_file_path.join("mod").with_extension(&self.extension);

        let (found_file_path, found_file) = match (
            Self::find(
                FileContentSource::File(mod_file_path.clone()),
                self.ancestry.last().cloned().map(|id| (id, &item_mod)),
            ),
            Self::find(
                FileContentSource::File(mod_folder_file_path.clone()),
                self.ancestry.last().cloned().map(|id| (id, &item_mod)),
            ),
        ) {
            (Ok(found_file), Err(_)) => (mod_file_path, found_file),
            (Err(_), Ok(found_file)) => (mod_folder_file_path, found_file),
            (Ok(found_file), Ok(_found_mod_file)) => {
                self.errors.push(error::conflicting_mod_files(
                    span.parent_file_id,
                    &item_mod,
                    mod_file_path.clone(),
                    mod_folder_file_path,
                ));
                // Create an error, but assume name.rhdl is the correct one and keep going
                (mod_file_path, found_file)
            }
            (Err(err1), Err(err2)) => {
                // match (err1, err2) {
                //     (
                //         FileFindingError::IoError(wrapped_io_error1),
                //         FileFindingError::IoError(wrapped_io_error2),
                //     ) => {
                //         // refinement: only give not found for the name.rhdl file
                //         if wrapped_io_error1.cause.kind() == std::io::ErrorKind::NotFound
                //             && wrapped_io_error2.cause.kind() == std::io::ErrorKind::NotFound
                //         {
                //             self.errors.push(wrapped_io_error1.into());
                //         } else {
                //             if wrapped_io_error1.cause.kind() != std::io::ErrorKind::NotFound {
                //                 self.errors.push(wrapped_io_error1.into());
                //             }
                //             if wrapped_io_error2.cause.kind() != std::io::ErrorKind::NotFound {
                //                 self.errors.push(wrapped_io_error2.into());
                //             }
                //         }
                //     }
                //     (err1, FileFindingError::IoError(wrapped_io_error2)) => {
                //         self.errors.push(err1);
                //         if wrapped_io_error2.cause.kind() != std::io::ErrorKind::NotFound {
                //             self.errors.push(wrapped_io_error2.into());
                //         }
                //     }
                //     (FileFindingError::IoError(wrapped_io_error1), err2) => {
                //         if wrapped_io_error1.cause.kind() != std::io::ErrorKind::NotFound {
                //             self.errors.push(wrapped_io_error1.into());
                //         }
                //         self.errors.push(err2);
                //     }
                //     (err1, err2) => {
                //         // Non IO errors indicate parsing + duplicate error...
                //         self.errors.push(err1);
                //         self.errors.push(err2);
                //         self.errors.push(
                //             DuplicateError {
                //                 file_path: mod_file_path,
                //                 folder_path: mod_folder_file_path,
                //                 span,
                //             }
                //             .into(),
                //         );
                //     }
                // }
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
                .add_edge(parent, span.ident_path.clone(), idx);
        }

        self.ancestry.push(idx);
        for m in mods {
            let module_span = m.span();
            if m.content.is_file() {
                self.find_mod(
                    m,
                    SpanSource {
                        parent_file_id: idx,
                        ident_path: span.ident_path.clone(),
                    },
                );
            } else {
                self.find_mod_with_content(
                    m,
                    SpanSource {
                        parent_file_id: idx,
                        ident_path: span.ident_path.clone(),
                    },
                );
            }
        }
        self.ancestry.pop();
    }

    /// A mod in a file can have declared sub-mods in files in ./mod/sub-mod.rs
    fn find_mod_with_content(&mut self, item_mod: ItemMod, mut span: SpanSource) {
        if let ModContent::Here(here) = item_mod.content {
            span.ident_path.push(item_mod.ident);
            for item in here.items {
                if let Item::Mod(m) = item {
                    if m.content.is_file() {
                        self.find_mod(
                            m,
                            SpanSource {
                                parent_file_id: span.parent_file_id,
                                ident_path: span.ident_path.clone(),
                            },
                        );
                    } else {
                        self.find_mod_with_content(
                            m,
                            SpanSource {
                                parent_file_id: span.parent_file_id,
                                ident_path: span.ident_path.clone(),
                            },
                        );
                    }
                }
            }
        }
    }

    fn find(
        mut src: FileContentSource,
        parent: Option<(FileId, &ItemMod)>,
    ) -> Result<File, Diagnostic<FileId>> {
        let content = match &mut src {
            FileContentSource::File(path) => fs::File::open(&path).and_then(|mut f| {
                let mut content = String::new();
                f.read_to_string(&mut content)?;
                Ok(content)
            }),
            FileContentSource::Reader(_, reader) => {
                let mut content = String::new();
                reader.read_to_string(&mut content).map(|_| content)
            }
        };
        match content {
            Ok(content) => match FileParser::new().parse(&content) {
                Err(err) => Err(error::parse(src.name(), parent, err)),
                Ok(parsed) => Ok(File {
                    src,
                    content,
                    parsed,
                    parent: parent.map(|(id, _)| id),
                }),
            },
            Err(cause) => Err(error::wrapped_io(src.name(), parent, cause)),
        }
    }
}
