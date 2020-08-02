use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;

use petgraph::{graph::NodeIndex, Graph};
use syn::{spanned::Spanned, Ident, Item, ItemMod};

use crate::error::{
    DuplicateError, FileFindingError, PreciseSynParseError, SpanSource, WorkingDirectoryError,
    WrappedIoError,
};

#[derive(Debug, Clone)]
pub struct File {
    pub content: String,
    pub syn: syn::File,
    pub src: FileContentSource,
}

pub type FileGraph = Graph<Rc<File>, Vec<Ident>>;

const STDIN_FALLBACK_EXTENSION: &str = "rhdl";

/// Finds source code for modules from their files recursively
/// Errors are related to file-reading issues, missing content, or duplicate files
/// Does not care about naming conflicts, as those are delegated to `ScopeBuilder`.
#[derive(Default)]
pub struct FileFinder {
    pub file_graph: FileGraph,
    pub errors: Vec<FileFindingError>,
    cwd: PathBuf,
    ancestry: Vec<NodeIndex>,
    extension: String,
}

#[derive(Debug, Clone)]
pub enum FileContentSource {
    File(PathBuf),
    Stdin,
}

impl FileFinder {
    /// A top level entry point
    /// TODO: handle a top level file named `a.rhdl` with `mod a;` declared.
    pub fn find_tree(&mut self, src: FileContentSource) {
        let path = match &src {
            FileContentSource::File(path) => Some(path.clone()),
            _ => None,
        };
        match Self::find(src, None) {
            Ok(file) => {
                let idx = self.file_graph.add_node(file.into());
                self.ancestry.push(idx);
                let mods: Vec<ItemMod> = self.file_graph[idx]
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
                    let file = self.file_graph[idx].clone();
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
            ),
            Self::find(
                FileContentSource::File(mod_folder_file_path.clone()),
                Some(span.clone()),
            ),
        ) {
            (Ok(found_file), Err(_)) | (Err(_), Ok(found_file)) => found_file,
            (Ok(found_file), Ok(found_mod_file)) => {
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
                self.errors.push(err1);
                if let FileFindingError::IoError(wrapped_io_error) = &err2 {
                    // Only give error 2 if it's NOT file not found
                    if wrapped_io_error.cause.kind() != std::io::ErrorKind::NotFound {
                        self.errors.push(err2);
                    }
                } else {
                    self.errors.push(err2);
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
        let idx = self.file_graph.add_node(found_file.into());
        if let Some(parent) = self.ancestry.last() {
            // Ok to use the cloned ident
            self.file_graph
                .add_edge(*parent, idx, span.ident_path.clone());
        }

        self.ancestry.push(idx);
        for m in mods {
            let module_span = m.span();
            let file = self.file_graph[idx].clone();
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

    fn find(src: FileContentSource, span: Option<SpanSource>) -> Result<File, FileFindingError> {
        let content = match &src {
            FileContentSource::File(path) => {
                let mut f = fs::File::open(&path).map_err(|cause| WrappedIoError {
                    src: src.clone(),
                    cause,
                    span: span.clone(),
                })?;
                let mut content = String::new();
                f.read_to_string(&mut content)
                    .map_err(|cause| WrappedIoError {
                        src: src.clone(),
                        cause,
                        span: span.clone(),
                    })?;
                content
            }
            FileContentSource::Stdin => {
                let mut stdin = std::io::stdin();
                let mut content = String::new();
                stdin
                    .read_to_string(&mut content)
                    .map_err(|cause| WrappedIoError {
                        src: src.clone(),
                        cause,
                        span,
                    })?;
                content
            }
        };
        match syn::parse_file(&content) {
            Err(err) => Err(PreciseSynParseError {
                cause: err,
                code: content,
                src,
            }
            .into()),
            Ok(syn) => Ok(File { src, content, syn }),
        }
    }
}