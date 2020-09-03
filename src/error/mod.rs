use std::fmt::{Display, Error, Formatter, Result};
use std::path::PathBuf;
use std::rc::Rc;

use colored::Colorize;
use proc_macro2::Span;
use syn::Ident;

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
                    _ => "N/A".to_string()
                }
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter) -> Result {
                match self {
                    $(
                        Self::$err(err) => {
                            write!(f, "{}", err)
                        },
                    )*
                    _ => write!(f, "N/A")
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
    fn fmt(&self, f: &mut Formatter) -> Result {
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
    pub ident_path: Vec<Ident>,
}

#[derive(Debug)]
pub struct DuplicateError {
    pub file_path: PathBuf,
    pub folder_path: PathBuf,
    pub span: SpanSource,
}

impl Display for DuplicateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    pub hint: DuplicateHint,
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
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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

impl Display for MultipleDefinitionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        render_location(
            f,
            format!(
                "the {} `{}` is {} multiple times",
                self.hint,
                self.name,
                if self.hint == DuplicateHint::NameBinding {
                    "bound"
                } else {
                    "defined"
                }
            ),
            (
                Reference::Error,
                &format!(
                    "`{}` {} here",
                    self.name,
                    if self.hint == DuplicateHint::NameBinding {
                        "rebound"
                    } else {
                        "redefined"
                    }
                ),
                self.duplicate,
            ),
            vec![(
                Reference::Info,
                &format!(
                    "previous {} of the {} `{}` here",
                    if self.hint == DuplicateHint::NameBinding {
                        "binding"
                    } else {
                        "definition"
                    },
                    self.hint,
                    self.name
                ),
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
    pub path_ident: Ident,
}

impl Display for SpecialIdentNotAtStartOfPathError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    pub ident: Ident,
    pub src: AmbiguitySource,
}

#[derive(Debug)]
pub enum AmbiguitySource {
    Item(ItemHint),
    Glob,
}

impl Display for AmbiguitySource {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        use AmbiguitySource::*;
        match self {
            Item(hint) => write!(f, "{}", hint),
            Glob => write!(f, "glob"),
        }
    }
}

impl Display for DisambiguationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        render_location(
            f,
            format!(
                "`{}` is ambiguous ({} versus other {}s found during resolution)",
                self.ident, self.src, self.src
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
    pub previous_ident: Option<Ident>,
    pub unresolved_ident: Ident,
    pub hint: ItemHint,
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
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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

impl Display for UnresolvedItemError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        let reference_msg = match self.previous_ident.as_ref() {
            Some(previous_ident) => format!(
                "no `{}` {} in `{}`",
                self.unresolved_ident, self.hint, previous_ident
            ),
            None => format!("no `{}` {}", self.unresolved_ident, self.hint),
        };
        render_location(
            f,
            format!("unresolved {} `{}`", self.hint, self.unresolved_ident),
            (
                Reference::Error,
                &reference_msg,
                self.unresolved_ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct UnexpectedItemError {
    pub file: Rc<File>,
    pub ident: Ident,
    pub actual_hint: ItemHint,
    pub expected_hint: ItemHint,
}

impl Display for UnexpectedItemError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        render_location(
            f,
            format!(
                "expected {}, found {} `{}`",
                self.expected_hint, self.actual_hint, self.ident
            ),
            (
                Reference::Error,
                &format!("not a {}", self.expected_hint),
                self.ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct SelfUsageError {
    pub file: Rc<File>,
    pub name_ident: Ident,
    pub cause: SelfUsageErrorCause,
}
#[derive(Debug)]
pub enum SelfUsageErrorCause {
    NotInGroup,
    InGroupAtRoot,
}

impl Display for SelfUsageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        render_location(
            f,
            match self.cause {
                SelfUsageErrorCause::InGroupAtRoot => format!(
                    "`{}` imports are only allowed in a {{ }} list with a non-empty prefix",
                    self.name_ident
                ),
                SelfUsageErrorCause::NotInGroup => format!(
                    "`{}` imports are only allowed within a {{ }} list",
                    self.name_ident
                ),
            },
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
    pub ident: Ident,
}

impl Display for TooManySupersError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
/// this way, there can be a
/// "item `b` is defined here" reference wherever the item is defined
#[derive(Debug)]
pub struct ItemVisibilityError {
    pub file: Rc<File>,
    pub ident: Ident,
    pub hint: ItemHint,
}

impl Display for ItemVisibilityError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        render_location(
            f,
            format!("{} `{}` is private", self.hint, self.ident),
            (
                Reference::Error,
                &format!("private {}", self.hint),
                self.ident.span(),
            ),
            vec![],
            &self.file.src,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct ScopeVisibilityError {
    pub file: Rc<File>,
    pub ident: Ident,
    pub hint: ItemHint,
}

impl Display for ScopeVisibilityError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        render_location(
            f,
            format!("this item is not visible in {} `{}`", self.hint, self.ident),
            (
                Reference::Error,
                "not visible in this scope",
                self.ident.span(),
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
    pub ident: Ident,
}

impl Display for InvalidRawIdentifierError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    pub path_ident: Ident,
}

impl Display for GlobalPathCannotHaveSpecialIdentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    pub previous_ident: Option<Ident>,
}

impl Display for GlobAtEntryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        render_location(
            f,
            "cannot glob-import without a scope",
            (
                Reference::Error,
                if self.has_leading_colon {
                    "this would import all crates"
                } else if self
                    .previous_ident
                    .as_ref()
                    .map(|prev| prev == "super")
                    .unwrap_or_default()
                {
                    "this would re-import all local items"
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
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    pub segment_ident: Ident,
    pub prev_segment_ident: Option<Ident>,
}

impl Display for NonAncestralError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
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
    UnresolvedItemError => UnresolvedItemError,
    UnexpectedItemError => UnexpectedItemError,

    InvalidRawIdentifierError => InvalidRawIdentifierError,
    SpecialIdentNotAtStartOfPathError => SpecialIdentNotAtStartOfPathError,
    GlobalPathCannotHaveSpecialIdentError => GlobalPathCannotHaveSpecialIdentError,

    SelfUsageError => SelfUsageError,
    TooManySupersError => TooManySupersError,
    ItemVisibilityError => ItemVisibilityError,
    ScopeVisibilityError => ScopeVisibilityError,
    IncorrectVisibilityError => IncorrectVisibilityError,
    NonAncestralError => NonAncestralError,


    GlobAtEntryError => GlobAtEntryError,
    UnsupportedError => UnsupportedError,
});
error!(TypeError {});
