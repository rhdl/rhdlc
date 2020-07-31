use std::fmt;
use std::fmt::Display;
use std::path::PathBuf;
use std::rc::Rc;

use colored::Colorize;
use proc_macro2::Span;

use crate::resolve::{File, ResolutionSource};

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
    pub res: ResolutionSource,
    pub code: String,
}

impl Display for PreciseSynParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let msg = match &self.res {
            ResolutionSource::File(_) => "could not parse file",
            ResolutionSource::Stdin => "could not parse stdin",
        };
        render_location(
            f,
            msg,
            (Reference::Error, &self.cause.to_string(), self.cause.span()),
            vec![],
            &self.res,
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
            &self.span.file.source,
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
    pub res: ResolutionSource,
    pub span: Option<SpanSource>,
}
impl Display for WrappedIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        match &self.span {
            Some(span) => {
                let ident = span.ident_path.last().unwrap();

                let source = match &self.res {
                    ResolutionSource::Stdin => "<stdin>".to_string().into(),
                    ResolutionSource::File(path) => path.to_string_lossy(),
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
                    &span.file.source,
                    &span.file.content,
                )
            }
            None => {
                let path = match &self.res {
                    ResolutionSource::File(path) => path.to_string_lossy().into(),
                    ResolutionSource::Stdin => "<stdin>".to_string(),
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

error!(ResolveError {
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
                &format!("previous definition of the type `{}` here", self.name),
                self.original,
            ),
            vec![(
                Reference::Info,
                &format!("`{}` redefined here", self.name),
                self.duplicate,
            )],
            &self.file.source,
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
            &self.file.source,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct PathDisambiguationError {
    pub file: Rc<File>,
    pub path_ident: syn::Ident,
}

impl Display for PathDisambiguationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        render_location(
            f,
            format!(
                "`{}` is ambiguous (name vs any other name during import resolution)",
                self.path_ident
            ),
            (Reference::Error, "ambiguous name", self.path_ident.span()),
            vec![],
            &self.file.source,
            &self.file.content,
        )
    }
}

#[derive(Debug)]
pub struct UnresolvedImportError {
    pub file: Rc<File>,
    pub previous_idents: Vec<syn::Ident>,
    pub unresolved_ident: syn::Ident,
}

impl Display for UnresolvedImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        let (reference_msg, reference_span) = match self.previous_idents.len() {
            0 => (
                format!("no `{}` crate or mod", self.unresolved_ident),
                self.unresolved_ident.span(),
            ),
            _nonzero => (
                format!(
                    "no `{}` in `{}`",
                    self.unresolved_ident,
                    self.previous_idents
                        .last()
                        .map(|ident| ident.to_string())
                        .unwrap()
                ),
                self.unresolved_ident
                    .span()
                    .join(self.previous_idents.last().unwrap().span())
                    .unwrap(),
            ),
        };
        render_location(
            f,
            format!("unresolved import `{}`", self.unresolved_ident),
            (Reference::Error, &reference_msg, reference_span),
            vec![],
            &self.file.source,
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
            (
                Reference::Error,
                "",
                self.name_ident.span(),
            ),
            vec![],
            &self.file.source,
            &self.file.content,
        )
    }
}

error!(ScopeError {
    MultipleDefinitionError => MultipleDefinitionError,
    PathDisambiguationError => PathDisambiguationError,
    SpecialIdentNotAtStartOfPathError => SpecialIdentNotAtStartOfPathError,
    SelfNameNotInGroupError => SelfNameNotInGroupError,
    UnresolvedImportError => UnresolvedImportError,
});
