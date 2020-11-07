use rhdl::ast::Ident;
use rhdl::ast::Span;
use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use codespan::FileId;
use codespan_reporting::diagnostic::{Diagnostic as CodespanDiagnostic, Label};
use lalrpop_util::{lexer::Token, ParseError};
use rhdl::ast::{ItemMod, Spanned};

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

// #[derive(Debug)]
// pub struct MultipleDefinitionError {
//     pub file: Rc<File>,
//     pub name: String,
//     pub original: Span,
//     pub duplicate: Span,
//     pub hint: DuplicateHint,
// }

pub fn multiple_definition(
    file_id: FileId,
    original: &Ident,
    duplicate: &Ident,
    hint: DuplicateHint,
) -> Diagnostic {
    Diagnostic::error().with_message(&format!(
        "the {} `{}` is {} multiple times",
        hint,
        original.inner,
        if hint == DuplicateHint::NameBinding {
            "bound"
        } else {
            "defined"
        }
    ))
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

// impl Display for MultipleDefinitionError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!(
//                 "the {} `{}` is {} multiple times",
//                 self.hint,
//                 self.name,
//                 if self.hint == DuplicateHint::NameBinding {
//                     "bound"
//                 } else {
//                     "defined"
//                 }
//             ),
//             (
//                 Reference::Error,
//                 &format!(
//                     "`{}` {} here",
//                     self.name,
//                     if self.hint == DuplicateHint::NameBinding {
//                         "rebound"
//                     } else {
//                         "redefined"
//                     }
//                 ),
//                 self.duplicate,
//             ),
//             vec![(
//                 Reference::Info,
//                 &format!(
//                     "previous {} of the {} `{}` here",
//                     if self.hint == DuplicateHint::NameBinding {
//                         "binding"
//                     } else {
//                         "definition"
//                     },
//                     self.hint,
//                     self.name
//                 ),
//                 self.original,
//             )],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct SpecialIdentNotAtStartOfPathError {
//     pub file: Rc<File>,
//     pub path_ident: Ident,
// }

// impl Display for SpecialIdentNotAtStartOfPathError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!(
//                 "`{}` in paths can only be used in the start position",
//                 self.path_ident
//             ),
//             (
//                 Reference::Error,
//                 &format!(
//                     "`{}` in paths can only be used in the start position",
//                     self.path_ident
//                 ),
//                 self.path_ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct DisambiguationError {
//     pub file: Rc<File>,
//     pub ident: Ident,
//     pub src: AmbiguitySource,
// }

// #[derive(Debug)]
// pub enum AmbiguitySource {
//     Item(ItemHint),
//     Glob,
// }

// impl Display for AmbiguitySource {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         use AmbiguitySource::*;
//         match self {
//             Item(hint) => write!(f, "{}", hint),
//             Glob => write!(f, "glob"),
//         }
//     }
// }

// impl Display for DisambiguationError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!(
//                 "`{}` is ambiguous ({} versus other {}s found during resolution)",
//                 self.ident, self.src, self.src
//             ),
//             (Reference::Error, "ambiguous name", self.ident.span()),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct UnresolvedItemError {
//     pub file: Rc<File>,
//     pub previous_ident: Option<Ident>,
//     pub unresolved_ident: Ident,
//     pub hint: ItemHint,
// }

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

// impl Display for UnresolvedItemError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         let reference_msg = match self.previous_ident.as_ref() {
//             Some(previous_ident) => format!(
//                 "no `{}` {} in `{}`",
//                 self.unresolved_ident, self.hint, previous_ident
//             ),
//             None => format!("no `{}` {}", self.unresolved_ident, self.hint),
//         };
//         render_location(
//             f,
//             format!("unresolved {} `{}`", self.hint, self.unresolved_ident),
//             (
//                 Reference::Error,
//                 &reference_msg,
//                 self.unresolved_ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct UnexpectedItemError {
//     pub file: Rc<File>,
//     pub ident: Ident,
//     pub actual_hint: ItemHint,
//     pub expected_hint: ItemHint,
// }

// impl Display for UnexpectedItemError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!(
//                 "expected {}, found {} `{}`",
//                 self.expected_hint, self.actual_hint, self.ident
//             ),
//             (
//                 Reference::Error,
//                 &format!("not a {}", self.expected_hint),
//                 self.ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct SelfUsageError {
//     pub file: Rc<File>,
//     pub name_ident: Ident,
//     pub cause: SelfUsageErrorCause,
// }
// #[derive(Debug)]
// pub enum SelfUsageErrorCause {
//     NotInGroup,
//     InGroupAtRoot,
// }

// impl Display for SelfUsageError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             match self.cause {
//                 SelfUsageErrorCause::InGroupAtRoot => format!(
//                     "`{}` imports are only allowed in a {{ }} list with a non-empty prefix",
//                     self.name_ident
//                 ),
//                 SelfUsageErrorCause::NotInGroup => format!(
//                     "`{}` imports are only allowed within a {{ }} list",
//                     self.name_ident
//                 ),
//             },
//             (Reference::Error, "", self.name_ident.span()),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct TooManySupersError {
//     pub file: Rc<File>,
//     pub ident: Ident,
// }

// impl Display for TooManySupersError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!("there are too many leading `{}` keywords", self.ident),
//             (
//                 Reference::Error,
//                 "goes beyond the crate root",
//                 self.ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// /// TODO: support references to other files
// /// this way, there can be a
// /// "item `b` is defined here" reference wherever the item is defined
// #[derive(Debug)]
// pub struct ItemVisibilityError {
//     pub file: Rc<File>,
//     pub ident: Ident,
//     pub hint: ItemHint,
// }

// impl Display for ItemVisibilityError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!("{} `{}` is private", self.hint, self.ident),
//             (
//                 Reference::Error,
//                 &format!("private {}", self.hint),
//                 self.ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct ScopeVisibilityError {
//     pub file: Rc<File>,
//     pub ident: Ident,
//     pub hint: ItemHint,
// }

// impl Display for ScopeVisibilityError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!("this item is not visible in {} `{}`", self.hint, self.ident),
//             (
//                 Reference::Error,
//                 "not visible in this scope",
//                 self.ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

pub fn invalid_raw_identifier(file_id: FileId, ident: &Ident) -> Diagnostic {
    Diagnostic::error()
        .with_message("`{}` cannot be a raw identifier")
        .with_labels(vec![Label::primary(file_id, ident.span())])
}

// #[derive(Debug)]
// pub struct GlobalPathCannotHaveSpecialIdentError {
//     pub file: Rc<File>,
//     pub path_ident: Ident,
// }

// impl Display for GlobalPathCannotHaveSpecialIdentError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!("global paths cannot start with `{}`", self.path_ident),
//             (
//                 Reference::Error,
//                 &format!("global paths cannot start with `{}`", self.path_ident),
//                 self.path_ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct GlobAtEntryError {
//     pub file: Rc<File>,
//     pub star_span: Span,
//     pub has_leading_colon: bool,
//     pub previous_ident: Option<Ident>,
// }

// impl Display for GlobAtEntryError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             "cannot glob-import without a scope",
//             (
//                 Reference::Error,
//                 if self.has_leading_colon {
//                     "this would import all crates"
//                 } else if self
//                     .previous_ident
//                     .as_ref()
//                     .map(|prev| prev == "super")
//                     .unwrap_or_default()
//                 {
//                     "this would re-import all local items"
//                 } else {
//                     "this would re-import all crates and local items"
//                 },
//                 self.star_span,
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct IncorrectVisibilityError {
//     pub file: Rc<File>,
//     pub vis_span: proc_macro2::Span,
// }

// impl Display for IncorrectVisibilityError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             "incorrect visibility",
//             (
//                 Reference::Error,
//                 "expected crate, super, or an ancestral path",
//                 self.vis_span,
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// #[derive(Debug)]
// pub struct UnsupportedError {
//     pub file: Rc<File>,
//     pub span: proc_macro2::Span,
//     pub reason: &'static str,
// }

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

// #[derive(Debug)]
// pub struct NonAncestralError {
//     pub file: Rc<File>,
//     pub segment_ident: Ident,
//     pub prev_segment_ident: Option<Ident>,
// }

// impl Display for NonAncestralError {
//     fn fmt(&self, f: &mut Formatter<'_>) -> Result {
//         render_location(
//             f,
//             format!(
//                 "`{}` is not an ancestor of {}",
//                 self.segment_ident,
//                 match &self.prev_segment_ident {
//                     Some(prev) => format!("`{}`", prev),
//                     None => "this scope".to_string(),
//                 }
//             ),
//             (
//                 Reference::Error,
//                 "not an ancestor",
//                 self.segment_ident.span(),
//             ),
//             vec![],
//             &self.file.src,
//             &self.file.content,
//         )
//     }
// }

// error!(ResolutionError {
//     MultipleDefinitionError => MultipleDefinitionError,
//     DisambiguationError => DisambiguationError,
//     UnresolvedItemError => UnresolvedItemError,
//     UnexpectedItemError => UnexpectedItemError,

//     InvalidRawIdentifierError => InvalidRawIdentifierError,
//     SpecialIdentNotAtStartOfPathError => SpecialIdentNotAtStartOfPathError,
//     GlobalPathCannotHaveSpecialIdentError => GlobalPathCannotHaveSpecialIdentError,

//     SelfUsageError => SelfUsageError,
//     TooManySupersError => TooManySupersError,
//     ItemVisibilityError => ItemVisibilityError,
//     ScopeVisibilityError => ScopeVisibilityError,
//     IncorrectVisibilityError => IncorrectVisibilityError,
//     NonAncestralError => NonAncestralError,

//     GlobAtEntryError => GlobAtEntryError,
//     UnsupportedError => UnsupportedError,
// });
// error!(TypeError {});
