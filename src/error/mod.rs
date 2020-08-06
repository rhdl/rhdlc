use std::fmt;
use std::fmt::Display;
use std::path::PathBuf;
use std::rc::Rc;

use colored::Colorize;
use proc_macro2::Span;

use crate::find_file::{File, FileContentSource};

mod render;
use render::*;

macro_rules! error {
    ($name: ident { $($err: ident => $path: ty,)* }) => {
        #[derive(Debug)]
        pub enum $name {
            $($err($path),)*
        }

        $(
            impl From<$path> for $name {
                fn from(err: $path) -> Self {
                    Self::$err(err)
                }
            }
        )*

        impl $name {
            pub fn name(&self) -> String {
                match self {
                    $(
                        Self::$err(_) => stringify!($err).to_string(),
                    )*
                }
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                match self {
                    $(
                        Self::$err(err) => {
                            write!(formatter, "{}", err)
                        },
                    )*
                }
            }
        }

        impl From<$name> for Vec<$name> {
            fn from(other: $name) -> Self{ vec![other] }
        }
    };
}

#[derive(Debug)]
pub struct PreciseSynParseError {
    pub cause: syn::Error,
    pub src: FileContentSource,
    pub code: String,
}

impl Display for PreciseSynParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let msg = match &self.src {
            FileContentSource::File(_) => "could not parse file".to_string(),
            FileContentSource::Reader(name, _) => format!("could not parse {}", name),
        };
        render_location(
            f,
            &msg,
            (Reference::Error, &self.cause.to_string(), self.cause.span()),
            vec![],
            &self.src,
            &self.code,
        )
    }
}

#[derive(Debug, Clone)]
pub struct SpanSource {
    pub file: Rc<File>,
    pub span: Span,
    pub ident_path: Vec<syn::Ident>,
}

#[derive(Debug)]
pub struct DuplicateError {
    pub file_path: PathBuf,
    pub folder_path: PathBuf,
    pub span: SpanSource,
}

impl Display for DuplicateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        let ident = self.span.ident_path.last().unwrap();
        render_location(
            f,
            format!(
                "duplicates of module `{}` were found at {} and {}",
                ident,
                self.file_path.to_string_lossy(),
                self.folder_path.to_string_lossy()
            ),
            (Reference::Error, "", self.span.span),
            vec![],
            &self.span.file.src,
            &self.span.file.content,
        )
    }
}

#[derive(Debug)]
pub struct WorkingDirectoryError {
    pub cause: std::io::Error,
}

impl Display for WorkingDirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(
            f,
            "{error}{header}",
            error = "error".red().bold(),
            header = format!(
                ": couldn't get the current working directory {}",
                self.cause
            )
            .bold()
        )
    }
}

#[derive(Debug)]
pub struct WrappedIoError {
    pub cause: std::io::Error,
    pub src: FileContentSource,
    pub span: Option<SpanSource>,
}
impl Display for WrappedIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        match &self.span {
            Some(span) => {
                let ident = span.ident_path.last().unwrap();

                let source = match &self.src {
                    FileContentSource::Reader(name, _) => format!("<{}>", name).into(),
                    FileContentSource::File(path) => path.to_string_lossy(),
                };
                render_location(
                    f,
                    format!("file not found for module `{}`", ident),
                    (
                        Reference::Error,
                        &format!("{} : {}", source, self.cause),
                        span.span,
                    ),
                    vec![],
                    &span.file.src,
                    &span.file.content,
                )
            }
            None => {
                let path = match &self.src {
                    FileContentSource::File(path) => path.to_string_lossy().into(),
                    FileContentSource::Reader(name, _) => format!("<{}>", name),
                };
                writeln!(
                    f,
                    "{error}{header}",
                    error = "error".red().bold(),
                    header = format!(": couldn't read {} : {}", path, self.cause).bold(),
                )
            }
        }
    }
}

error!(FileFindingError {
    IoError => WrappedIoError,
    ParseError => PreciseSynParseError,
    WorkingDirectoryError => WorkingDirectoryError,
    DuplicateError => DuplicateError,
});

#[derive(Debug)]
pub struct MultipleDefinitionError {
    pub file: Rc<File>,
    pub name: String,
    pub original: Span,
    pub duplicate: Span,
}

impl Display for MultipleDefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!("the name `{}` is defined multiple times", self.name),
            (
                Reference::Error,
                &format!("`{}` redefined here", self.name),
                self.duplicate,
            ),
            vec![(
                Reference::Info,
                &format!("previous definition of the type `{}` here", self.name),
                self.original,
            )],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct SpecialIdentNotAtStartOfPathError {
    pub file: Rc<File>,
    pub path_ident: syn::Ident,
}

impl Display for SpecialIdentNotAtStartOfPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!(
                "`{}` in paths can only be used in the start position",
                self.path_ident
            ),
            (
                Reference::Error,
                &format!(
                    "`{}` in paths can only be used in the start position",
                    self.path_ident
                ),
                self.path_ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct DisambiguationError {
    pub file: Rc<File>,
    pub ident: syn::Ident,
    pub this: AmbiguitySource,
    pub other: AmbiguitySource,
}

#[derive(Debug)]
pub enum AmbiguitySource {
    Name,
    Glob,
}

impl Display for AmbiguitySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        use AmbiguitySource::*;
        match self {
            Name => write!(f, "name"),
            Glob => write!(f, "glob"),
        }
    }
}

impl Display for DisambiguationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!(
                "`{}` is ambiguous ({} versus other {}s found during resolution)",
                self.ident, self.this, self.other
            ),
            (Reference::Error, "ambiguous name", self.ident.span()),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct UnresolvedItemError {
    pub file: Rc<File>,
    pub previous_idents: Vec<syn::Ident>,
    pub has_leading_colon: bool,
    pub unresolved_ident: syn::Ident,
}

impl Display for UnresolvedItemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        let (reference_msg, reference_span) =
            match (self.previous_idents.len(), self.has_leading_colon) {
                (0, false) => (
                    format!("no `{}` crate or mod", self.unresolved_ident),
                    self.unresolved_ident.span(),
                ),
                (0, true) => (
                    format!("no `{}` external crate", self.unresolved_ident),
                    self.unresolved_ident.span(),
                ),
                (_nonzero, _) => (
                    format!(
                        "no `{}` in `{}`",
                        self.unresolved_ident,
                        self.previous_idents
                            .last()
                            .map(|ident| ident.to_string())
                            .unwrap()
                    ),
                    self.unresolved_ident.span(),
                ),
            };
        render_location(
            f,
            format!("unresolved item `{}`", self.unresolved_ident),
            (Reference::Error, &reference_msg, reference_span),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct SelfNameNotInGroupError {
    pub file: Rc<File>,
    pub name_ident: syn::Ident,
}

impl Display for SelfNameNotInGroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!(
                "`{}` imports are only allowed within a {{ }} list",
                self.name_ident
            ),
            (Reference::Error, "", self.name_ident.span()),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct TooManySupersError {
    pub file: Rc<File>,
    pub ident: syn::Ident,
}

impl Display for TooManySupersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!("there are too many leading `{}` keywords", self.ident),
            (
                Reference::Error,
                "goes beyond the crate root",
                self.ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

/// TODO: support references to other files
/// TODO: give the actual type of item
/// this way, there can be a
/// "item `b` is defined here" reference wherever the item is defined
#[derive(Debug)]
pub struct ItemVisibilityError {
    pub name_file: Rc<File>,
    pub name_ident: syn::Ident,
}

impl Display for ItemVisibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!("{} `{}` is private", "item", self.name_ident),
            (
                Reference::Error,
                &format!("private {}", "item"),
                self.name_ident.span(),
            ),
            vec![],
            &self.name_file.src,
            &self.name_file.content,
        )
    }
}

#[derive(Debug)]
pub struct ScopeVisibilityError {
    pub file: Rc<File>,
    pub scope_ident: syn::Ident,
}

impl Display for ScopeVisibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!("this item is private in `{}`", self.scope_ident),
            (
                Reference::Error,
                "private in this scope",
                self.scope_ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct InvalidRawIdentifierError {
    pub file: Rc<File>,
    pub ident: syn::Ident,
}

impl Display for InvalidRawIdentifierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!("`{}` cannot be a raw identifier", self.ident),
            (Reference::Error, "", self.ident.span()),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct GlobalPathCannotHaveSpecialIdentError {
    pub file: Rc<File>,
    pub path_ident: syn::Ident,
}

impl Display for GlobalPathCannotHaveSpecialIdentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!("global paths cannot start with `{}`", self.path_ident),
            (
                Reference::Error,
                &format!("global paths cannot start with `{}`", self.path_ident),
                self.path_ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct GlobAtEntryError {
    pub file: Rc<File>,
    pub star_span: Span,
    pub has_leading_colon: bool,
}

impl Display for GlobAtEntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            "cannot glob-import without a scope",
            (
                Reference::Error,
                if self.has_leading_colon {
                    "this would import all crates"
                } else {
                    "this would re-import all crates and local items"
                },
                self.star_span,
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct IncorrectVisibilityError {
    pub file: Rc<File>,
    pub vis_span: proc_macro2::Span,
}

impl Display for IncorrectVisibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            "incorrect visibility",
            (
                Reference::Error,
                "expected crate, super, or an ancestral path",
                self.vis_span,
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct UnsupportedError {
    pub file: Rc<File>,
    pub span: proc_macro2::Span,
    pub reason: &'static str,
}

impl Display for UnsupportedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            "unsupported feature",
            (Reference::Error, self.reason, self.span),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct NonAncestralError {
    pub file: Rc<File>,
    pub segment_ident: syn::Ident,
    pub prev_segment_ident: Option<syn::Ident>,
}

impl Display for NonAncestralError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!(
                "`{}` is not an ancestor of {}",
                self.segment_ident,
                match &self.prev_segment_ident {
                    Some(prev) => format!("`{}`", prev),
                    None => "this scope".to_string(),
                }
            ),
            (
                Reference::Error,
                "not an ancestor",
                self.segment_ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

error!(ResolutionError {
    MultipleDefinitionError => MultipleDefinitionError,
    DisambiguationError => DisambiguationError,
    SpecialIdentNotAtStartOfPathError => SpecialIdentNotAtStartOfPathError,
    SelfNameNotInGroupError => SelfNameNotInGroupError,
    UnresolvedItemError => UnresolvedItemError,
    TooManySupersError => TooManySupersError,
    ItemVisibilityError => ItemVisibilityError,
    InvalidRawIdentifierError => InvalidRawIdentifierError,
    GlobalPathCannotHaveSpecialIdentError => GlobalPathCannotHaveSpecialIdentError,
    GlobAtEntryError => GlobAtEntryError,
    IncorrectVisibilityError => IncorrectVisibilityError,
    UnsupportedError => UnsupportedError,
    NonAncestralError => NonAncestralError,
    ScopeVisibilityError => ScopeVisibilityError,
});
