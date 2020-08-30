use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;

use fnv::FnvHashMap as HashMap;
use syn::{spanned::Spanned, Ident, Item, ItemMod};

use crate::error::{
    DuplicateError, FileFindingError, PreciseSynParseError, SpanSource, WorkingDirectoryError,
    WrappedIoError,
};

#[derive(Debug)]
pub struct File {
    pub content: String,
    pub syn: syn::File,
    pub src: FileContentSource,
    pub parent: Option<FileGraphIndex>,
}

#[derive(Default, Debug)]
pub struct FileGraph {
    pub inner: Vec<Rc<File>>,
    pub roots: Vec<FileGraphIndex>,
    pub children: HashMap<FileGraphIndex, Vec<(Vec<Ident>, FileGraphIndex)>>,
}

impl FileGraph {
    fn add_node(&mut self, file: Rc<File>, parent: Option<FileGraphIndex>) -> FileGraphIndex {
        let idx = self.inner.len();
        self.inner.push(file);
        if let None = parent {
            self.roots.push(idx);
        }
        idx
    }

    fn add_edge(&mut self, parent: FileGraphIndex, idents: Vec<Ident>, child: FileGraphIndex) {
        self.children
            .entry(parent)
            .or_default()
            .push((idents, child));
    }

    pub fn node_indices(&self) -> impl Iterator<Item = FileGraphIndex> {
        0..self.inner.len()
    }
}

pub type FileGraphIndex = usize;

const STDIN_FALLBACK_EXTENSION: &str = "rhdl";

/// Finds source code for modules from their files recursively
/// Errors are related to file-reading issues, missing content, or duplicate files
/// Does not care about naming conflicts, as those are delegated to `ScopeBuilder`.
#[derive(Default)]
pub struct FileFinder {
    pub file_graph: FileGraph,
    pub errors: Vec<FileFindingError>,
    cwd: PathBuf,
    ancestry: Vec<FileGraphIndex>,
    extension: String,
}

pub enum FileContentSource {
    File(PathBuf),
    Reader(String, Box<dyn Read>),
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
        let path = match &src {
            FileContentSource::File(path) => Some(path.clone()),
            _ => None,
        };
        match Self::find(src, None, None) {
            Ok(file) => {
                let idx = self
                    .file_graph
                    .add_node(file.into(), self.ancestry.last().cloned());
                self.ancestry.push(idx);
                let mods: Vec<ItemMod> = self.file_graph.inner[idx]
                    .syn
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
                            self.errors.push(WorkingDirectoryError { cause }.into());
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
                    let file = self.file_graph.inner[idx].clone();
                    let module_span = m.span();
                    if m.content.is_none() {
                        self.find_mod(
                            m,
                            SpanSource {
                                file,
                                ident_path: vec![],
                                span: module_span,
                            },
                        );
                    } else {
                        self.find_mod_with_content(
                            m,
                            SpanSource {
                                file,
                                ident_path: vec![],
                                span: module_span,
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
        span.ident_path.push(item_mod.ident);
        let mut mod_file_path = self.cwd.clone();
        span.ident_path.iter().for_each(|ident| {
            let ident = ident.to_string();
            mod_file_path.push(ident.strip_prefix("r#").unwrap_or(&ident));
        });
        let mod_folder_file_path = mod_file_path.join("mod").with_extension(&self.extension);
        let mod_file_path = mod_file_path.with_extension(&self.extension);

        let found_file = match (
            Self::find(
                FileContentSource::File(mod_file_path.clone()),
                Some(span.clone()),
                self.ancestry.last().cloned(),
            ),
            Self::find(
                FileContentSource::File(mod_folder_file_path.clone()),
                Some(span.clone()),
                self.ancestry.last().cloned(),
            ),
        ) {
            (Ok(found_file), Err(_)) | (Err(_), Ok(found_file)) => found_file,
            (Ok(found_file), Ok(_found_mod_file)) => {
                self.errors.push(
                    DuplicateError {
                        file_path: mod_file_path,
                        folder_path: mod_folder_file_path,
                        span: span.clone(),
                    }
                    .into(),
                );
                // Create an error, but assume name.rhdl is the correct one and keep going
                found_file
            }
            (Err(err1), Err(err2)) => {
                match (err1, err2) {
                    (
                        FileFindingError::IoError(wrapped_io_error1),
                        FileFindingError::IoError(wrapped_io_error2),
                    ) => {
                        // refinement: only give not found for the name.rhdl file
                        if wrapped_io_error1.cause.kind() == std::io::ErrorKind::NotFound
                            && wrapped_io_error2.cause.kind() == std::io::ErrorKind::NotFound
                        {
                            self.errors.push(wrapped_io_error1.into());
                        } else {
                            if wrapped_io_error1.cause.kind() != std::io::ErrorKind::NotFound {
                                self.errors.push(wrapped_io_error1.into());
                            }
                            if wrapped_io_error2.cause.kind() != std::io::ErrorKind::NotFound {
                                self.errors.push(wrapped_io_error2.into());
                            }
                        }
                    }
                    (err1, FileFindingError::IoError(wrapped_io_error2)) => {
                        self.errors.push(err1);
                        if wrapped_io_error2.cause.kind() != std::io::ErrorKind::NotFound {
                            self.errors.push(wrapped_io_error2.into());
                        }
                    }
                    (FileFindingError::IoError(wrapped_io_error1), err2) => {
                        if wrapped_io_error1.cause.kind() != std::io::ErrorKind::NotFound {
                            self.errors.push(wrapped_io_error1.into());
                        }
                        self.errors.push(err2);
                    }
                    (err1, err2) => {
                        // Non IO errors indicate parsing + duplicate error...
                        self.errors.push(err1);
                        self.errors.push(err2);
                        self.errors.push(
                            DuplicateError {
                                file_path: mod_file_path,
                                folder_path: mod_folder_file_path,
                                span,
                            }
                            .into(),
                        );
                    }
                }
                return;
            }
        };

        let mods: Vec<ItemMod> = found_file
            .syn
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mod(m) => Some(m),
                _ => None,
            })
            .cloned()
            .collect();
        let idx = self
            .file_graph
            .add_node(found_file.into(), self.ancestry.last().cloned());
        if let Some(parent) = self.ancestry.last().cloned() {
            self.file_graph
                .add_edge(parent, span.ident_path.clone(), idx);
        }

        self.ancestry.push(idx);
        for m in mods {
            let module_span = m.span();
            let file = self.file_graph.inner[idx].clone();
            if m.content.is_none() {
                self.find_mod(
                    m,
                    SpanSource {
                        file,
                        ident_path: span.ident_path.clone(),
                        span: module_span,
                    },
                );
            } else {
                self.find_mod_with_content(
                    m,
                    SpanSource {
                        file,
                        ident_path: span.ident_path.clone(),
                        span: module_span,
                    },
                );
            }
        }
        self.ancestry.pop();
    }

    /// A mod in a file can have declared sub-mods in files in ./mod/sub-mod.rs
    fn find_mod_with_content(&mut self, item_mod: ItemMod, mut span: SpanSource) {
        if let Some(content) = item_mod.content {
            span.ident_path.push(item_mod.ident);
            for item in content.1 {
                if let Item::Mod(m) = item {
                    let module_span = m.span();
                    if m.content.is_none() {
                        self.find_mod(
                            m,
                            SpanSource {
                                file: span.file.clone(),
                                ident_path: span.ident_path.clone(),
                                span: module_span,
                            },
                        );
                    } else {
                        self.find_mod_with_content(
                            m,
                            SpanSource {
                                file: span.file.clone(),
                                ident_path: span.ident_path.clone(),
                                span: module_span,
                            },
                        );
                    }
                }
            }
        }
    }

    fn find(
        mut src: FileContentSource,
        span: Option<SpanSource>,
        parent: Option<FileGraphIndex>,
    ) -> Result<File, FileFindingError> {
        let content = match &mut src {
            FileContentSource::File(path) => fs::File::open(&path).and_then(|mut f| {
                let mut content = String::new();
                f.read_to_string(&mut content)?;
                Ok(content)
            }),
            FileContentSource::Reader(_, reader) => Ok(String::new()).and_then(|mut content| {
                reader.read_to_string(&mut content)?;
                Ok(content)
            }),
        };
        match content {
            Ok(content) => match syn::parse_file(&content) {
                Err(err) => Err(PreciseSynParseError {
                    cause: err,
                    code: content,
                    src,
                }
                .into()),
                Ok(syn) => Ok(File {
                    src,
                    content,
                    syn,
                    parent,
                }),
            },
            Err(cause) => Err(WrappedIoError { cause, src, span }.into()),
        }
    }
}
