use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use codespan::FileId;
use codespan_reporting::diagnostic::{Diagnostic as CodespanDiagnostic, Label};
use lalrpop_util::{lexer::Token, ParseError};
use rhdl::ast::{Ident, ItemMod, PathSep, Span, Spanned, UseTreeGlob};

pub type Diagnostic = CodespanDiagnostic<FileId>;

pub enum FileFindingError {
    Parse(Diagnostic),
    Io(std::io::Error),
}

impl FileFindingError {
    pub fn is_io_not_found(&self) -> bool {
        match self {
            Self::Io(err) => err.kind() == std::io::ErrorKind::NotFound,
            Self::Parse(_) => false,
        }
    }
    pub fn diagnostic(self, name: OsString, parent: Option<(FileId, &ItemMod)>) -> Diagnostic {
        match self {
            Self::Parse(diag) => diag,
            Self::Io(err) => Diagnostic::error()
                .with_message(format!("couldn't read {}: {}", name.to_string_lossy(), err,))
                .with_labels({
                    let mut labels = vec![];
                    if let Some((parent_file_id, this_item_mod_decl)) = parent {
                        labels.push(
                            Label::primary(parent_file_id, this_item_mod_decl.span())
                                .with_message("declared here"),
                        );
                    }
                    labels
                }),
        }
    }
}

pub fn parse<'input>(
    name: OsString,
    file_id: FileId,
    parent: Option<(FileId, &ItemMod)>,
    err: ParseError<usize, Token<'input>, &'static str>,
) -> Diagnostic {
    use ParseError::*;

    Diagnostic::error()
        .with_message(format!(
            "could not parse {}: {}",
            name.to_string_lossy(),
            match &err {
                UnrecognizedToken { .. } => "unexpected token",
                UnrecognizedEOF { .. } => "unexpected EOF",
                InvalidToken { .. } => "invalid token",
                ExtraToken { .. } => "extra token",
                User { error } => error,
            }
        ))
        .with_labels({
            let mut labels = vec![];
            match err {
                UnrecognizedToken {
                    token: (left, _token, right),
                    expected,
                } => labels.push(
                    Label::primary(file_id, left..right)
                        .with_message(format!("expected any of {:?}", expected)),
                ),
                UnrecognizedEOF { location, expected } => labels.push(
                    Label::primary(file_id, location..location)
                        .with_message(format!("expected any of {:?}", expected)),
                ),
                InvalidToken { location } => {
                    labels.push(Label::primary(file_id, location..location))
                }
                ExtraToken {
                    token: (left, _token, right),
                } => labels.push(Label::primary(file_id, left..right)),
                User { .. } => {}
            }
            if let Some((parent_file_id, this_item_mod_decl)) = parent {
                labels.push(
                    Label::secondary(parent_file_id, this_item_mod_decl.span())
                        .with_message("declared here"),
                );
            }
            labels
        })
}

pub fn conflicting_mod_files(
    parent_file_id: Option<FileId>,
    item_mod: &ItemMod,
    file_path: &PathBuf,
    folder_path: &PathBuf,
) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!(
            "conflicting files for module `{}` were found at {} and {}",
            item_mod.ident,
            file_path.to_string_lossy(),
            folder_path.to_string_lossy()
        ))
        .with_labels({
            let mut labels = vec![];
            if let Some(parent_file_id) = parent_file_id {
                labels.push(
                    Label::primary(parent_file_id, item_mod.span()).with_message("declared here"),
                );
            }
            labels
        })
}

pub fn working_directory(cause: std::io::Error) -> Diagnostic {
    Diagnostic::error().with_message(format!(
        "couldn't get the current working directory: {}",
        cause,
    ))
}

pub fn multiple_definition(
    file_id: FileId,
    original: &Ident,
    duplicate: &Ident,
    hint: DuplicateHint,
) -> Diagnostic {
    Diagnostic::error()
        .with_message(&format!(
            "the {} `{}` is {} multiple times",
            hint,
            original.inner,
            if hint == DuplicateHint::NameBinding {
                "bound"
            } else {
                "defined"
            }
        ))
        .with_labels(vec![
            Label::primary(file_id, duplicate.span()).with_message(&format!(
                "`{}` {} here",
                duplicate,
                if hint == DuplicateHint::NameBinding {
                    "rebound"
                } else {
                    "redefined"
                }
            )),
            Label::secondary(file_id, original.span()).with_message(&format!(
                "previous {} of the {} `{}` here",
                if hint == DuplicateHint::NameBinding {
                    "binding"
                } else {
                    "definition"
                },
                hint,
                duplicate,
            )),
        ])
}

#[derive(Debug, PartialEq, Eq)]
pub enum DuplicateHint {
    Variant,
    Name,
    Lifetime,
    TypeParam,
    Field,
    NameBinding,
}

impl Display for DuplicateHint {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use DuplicateHint::*;
        match self {
            Variant => write!(f, "variant"),
            Name => write!(f, "name"),
            Lifetime => write!(f, "lifetime"),
            TypeParam => write!(f, "type parameter"),
            Field => write!(f, "field"),
            NameBinding => write!(f, "name"),
        }
    }
}

pub fn special_ident_not_at_start_of_path(file_id: FileId, path_ident: &Ident) -> Diagnostic {
    Diagnostic::error()
        .with_message(&format!(
            "special identifier `{}` can only be in the start position of a path",
            path_ident
        ))
        .with_labels(vec![Label::primary(file_id, path_ident.span())])
}

pub fn disambiguation_needed(file_id: FileId, ident: &Ident, src: AmbiguitySource) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!(
            "`{}` is ambiguous ({} versus other {}s found during resolution)",
            ident, src, src
        ))
        .with_labels(vec![
            Label::primary(file_id, ident.span()).with_message("ambiguous name")
        ])
        .with_notes(vec![
            format!("rename other {}s with the same name", src),
            "if there's an import, you can rename it like use std::path::Path as StdPath;"
                .to_string(),
        ])
}

#[derive(Debug)]
pub enum AmbiguitySource {
    Item(ItemHint),
    Glob,
}

impl Display for AmbiguitySource {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use AmbiguitySource::*;
        match self {
            Item(hint) => write!(f, "{}", hint),
            Glob => write!(f, "glob"),
        }
    }
}

pub fn unresolved_item(
    file_id: FileId,
    previous_ident: Option<&Ident>,
    unresolved_ident: &Ident,
    hint: ItemHint,
    possibilities: Vec<Vec<&str>>,
) -> Diagnostic {
    let reference_msg = match previous_ident {
        Some(previous_ident) => {
            format!("no `{}` {} in `{}`", unresolved_ident, hint, previous_ident)
        }
        None => format!("no `{}` {}", unresolved_ident, hint),
    };
    let mut notes = vec![];
    if !possibilities.is_empty() {
        notes.push("Did you mean:".to_string());
    }
    notes.extend(possibilities.iter().map(|path| {
        let mut acc = String::new();
        for segment in path.iter().take(path.len().saturating_sub(1)) {
            acc += segment;
            acc += "::";
        }
        if let Some(last) = path.last() {
            acc += last;
        }
        acc
    }));
    Diagnostic::error()
        .with_message(&format!("unresolved {} `{}`", hint, unresolved_ident))
        .with_labels(vec![
            Label::primary(file_id, unresolved_ident.span()).with_message(reference_msg)
        ])
        .with_notes(notes)
}

#[derive(Debug)]
pub enum ItemHint {
    /// mod
    InternalNamedChildScope,
    /// root
    InternalNamedRootScope,
    /// crate
    ExternalNamedScope,
    /// mod or crate
    InternalNamedChildOrExternalNamedScope,
    /// any item
    Item,
    /// a trait in particular
    Trait,
    /// any type (alias, struct, enum, or other)
    Type,
    /// any variable (const or static)
    Var,
    /// a method or function
    Fn,
}

impl Display for ItemHint {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use ItemHint::*;
        match self {
            InternalNamedChildScope => write!(f, "mod"),
            InternalNamedRootScope => write!(f, "root"),
            ExternalNamedScope => write!(f, "crate"),
            InternalNamedChildOrExternalNamedScope => write!(f, "crate or mod"),
            Item => write!(f, "item"),
            Trait => write!(f, "trait"),
            Type => write!(f, "type"),
            Var => write!(f, "variable"),
            Fn => write!(f, "function"),
        }
    }
}

pub fn unexpected_item(
    file_id: FileId,
    ident: &Ident,
    actual_hint: ItemHint,
    expected_hint: ItemHint,
) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!(
            "expected {}, found {} `{}`",
            expected_hint, actual_hint, ident
        ))
        .with_labels(vec![
            Label::primary(file_id, ident.span()).with_message(&format!("not a {}", expected_hint))
        ])
}

pub fn self_usage(file_id: FileId, name_ident: &Ident, cause: SelfUsageErrorCause) -> Diagnostic {
    Diagnostic::error()
        .with_message(match cause {
            SelfUsageErrorCause::InGroupAtRoot => format!(
                "`{}` imports are only allowed in a braced list with a non-empty prefix",
                name_ident
            ),
            SelfUsageErrorCause::NotInGroup => format!(
                "`{}` imports are only allowed within a braced list",
                name_ident
            ),
        })
        .with_labels(vec![Label::primary(file_id, name_ident.span())
            .with_message(match cause {
                SelfUsageErrorCause::NotInGroup => "surround this with curly braces",
                SelfUsageErrorCause::InGroupAtRoot => "this makes no sense",
            })])
}

#[derive(Debug)]
pub enum SelfUsageErrorCause {
    NotInGroup,
    InGroupAtRoot,
}

pub fn too_many_supers(file_id: FileId, ident: &Ident) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!("there are too many leading `{}` keywords", ident))
        .with_labels(vec![
            Label::primary(file_id, ident.span()).with_message("goes beyond the crate root")
        ])
        .with_notes(vec![format!("try removing that `{}` from the path", ident)])
}

pub fn item_visibility(
    file_id: FileId,
    ident: &Ident,
    declaration_file_id: FileId,
    declaration_ident: &Ident,
    hint: ItemHint,
) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!("{} `{}` is private", hint, ident))
        .with_labels(vec![
            Label::primary(file_id, ident.span()).with_message(format!("{} is private", hint)),
            Label::secondary(declaration_file_id, declaration_ident.span())
                .with_message("declared here"),
        ])
        .with_notes(vec![format!(
            "modify the visibility of `{}` if you want to use it",
            ident
        )])
}

pub fn scope_visibility(file_id: FileId, ident: &Ident, hint: ItemHint) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!("item is not visible in {} `{}`", hint, ident))
        .with_labels(vec![
            Label::primary(file_id, ident.span()),
        ])
        .with_notes(vec![format!("modify the visibility of the immediate child module of `{}` that is an ancestor of this item", ident)])
}

pub fn invalid_raw_identifier(file_id: FileId, ident: &Ident) -> Diagnostic {
    Diagnostic::error()
        .with_message("`{}` cannot be a raw identifier")
        .with_labels(vec![Label::primary(file_id, ident.span())])
}

pub fn global_path_cannot_have_special_ident(
    file_id: FileId,
    path_ident: &Ident,
    leading_sep: &PathSep,
) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!("global paths cannot start with `{}`", path_ident))
        .with_labels(vec![
            Label::primary(file_id, path_ident.span()),
            Label::secondary(file_id, leading_sep.span()).with_message("makes this path global"),
        ])
        .with_notes(vec![
            "remove the leading path separator to make this path local".to_string(),
            "if this is meant to be global, add the crate name after the leading separator"
                .to_string(),
            format!("`{}` is not a valid crate name", path_ident),
        ])
}

pub fn glob_at_entry(
    file_id: FileId,
    glob: &UseTreeGlob,
    leading_sep: Option<&PathSep>,
    previous_ident: Option<&Ident>,
) -> Diagnostic {
    Diagnostic::error()
        .with_message("cannot glob-import without a scope")
        .with_labels(vec![Label::primary(file_id, glob.span()).with_message(
            if leading_sep.is_some() {
                "this would import all crates"
            } else if previous_ident
                .as_ref()
                .map(|prev| *prev == "super")
                .unwrap_or_default()
            {
                "this would re-import all local items"
            } else {
                "this would re-import all crates and local items"
            },
        )])
}

pub fn incorrect_visibility_restriction(file_id: FileId, span: Span) -> Diagnostic {
    Diagnostic::error()
        .with_message("incorrect visibility restriction")
        .with_labels(vec![Label::primary(file_id, span)
            ])
            .with_notes(vec!["visibility can only be restricted to a local ancestral scope: crate, super, or a path beginning with the former two".to_string()])
}

pub fn module_with_external_file_in_fn(file_id: FileId, item_mod: &ItemMod) -> Diagnostic {
    Diagnostic::error()
        .with_message("a module in a function cannot be loaded from an external file")
        .with_labels(vec![
            Label::primary(file_id, item_mod.span()).with_message("give the module a body")
        ])
        .with_notes(vec![
            "the file path of such a module would be ambiguous".to_string()
        ])
}

pub fn non_ancestral_visibility(
    file_id: FileId,
    segment_ident: &Ident,
    prev_segment_ident: Option<&Ident>,
) -> Diagnostic {
    Diagnostic::error()
        .with_message(format!(
            "`{}` is not an ancestor of {}",
            segment_ident,
            match prev_segment_ident {
                Some(prev) => format!("`{}`", prev),
                None => "this scope".to_string(),
            }
        ))
        .with_labels(vec![Label::primary(file_id, segment_ident.span())])
        .with_notes(vec![
            "visibility can only be restricted to an ancestral path".to_string(),
        ])
}
