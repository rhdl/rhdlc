use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use syn::visit_mut::{self, VisitMut};
use syn::{parse_quote, ItemMod};

use log::info;

use crate::error::{ResolveError, PreciseSynParseError};

/// Resolves source code for modules from their files recursively
/// Errors are related to file-reading issues or missing content
pub struct ModResolver<'a> {
    pub cwd: &'a Path,
    pub errors: Vec<ResolveError>,
}

impl<'a> VisitMut for ModResolver<'a> {
    /// If the code is in a mod.rhdl file, there could be more modules that need to be recursively resolved.
    fn visit_item_mod_mut(&mut self, item_mod: &mut ItemMod) {
        if item_mod.content.is_some() {
            return;
        }

        let ident = &item_mod.ident;
        let mod_file_path = self.cwd.join(format!("{}.rhdl", ident));
        let mod_folder_file_path = self.cwd.join(format!("{}/mod.rhdl", ident));

        let (path, in_folder): (PathBuf, bool) = if mod_file_path.is_file() {
            (mod_file_path, false)
        } else if mod_folder_file_path.is_file() {
            (mod_folder_file_path, true)
        } else {
            self.errors
                .push(ResolveError::NotFoundError(item_mod.ident.clone()));
            return;
        };

        let tree = Self::resolve_module_file(&path);
        match tree {
            Err(err) => {
                self.errors.push(err);
                return;
            }
            Ok(mut tree) => {
                if in_folder {
                    let folder = path.parent().unwrap();
                    let mut pull_folder = ModResolver::new(folder);
                    pull_folder.visit_file_mut(&mut tree);
                    self.errors.append(&mut pull_folder.errors);
                }
                *item_mod = parse_quote!(mod #ident { #tree });
            }
        }
    }
}

impl<'a> ModResolver<'a> {
    pub fn new(cwd: &'a Path) -> Self {
        Self {
            cwd,
            errors: vec![],
        }
    }

    fn resolve_module_file(path: &Path) -> Result<syn::File, ResolveError> {
        let mut file = File::open(&path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let tree = syn::parse_file(&content).map_err(|err| {
            PreciseSynParseError {
                cause: err,
                code: content,
                path: path.to_owned()
            }
        })?;
        Ok(tree)
    }
}
