use std::fmt;
use std::fmt::Display;
use std::path::PathBuf;

use proc_macro2::Span;

use colored::Colorize;

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
    pub path: PathBuf,
    pub code: String,
}

impl Display for PreciseSynParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        render_location(
            f,
            "could not parse file",
            (Reference::Error, &self.cause.to_string(), self.cause.span()),
            vec![],
            &self.path,
            &self.code,
        )
    }
}

#[derive(Debug)]
pub struct DuplicateError {
    pub ident: syn::Ident,
    pub file: PathBuf,
    pub folder: PathBuf,
}

impl Display for DuplicateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(
            f,
            "\n\
             {error}{header}\n\
             {indent}{arrow} {file}\n\
             {indent}{arrow} {folder}\n\
            ",
            error = "error".red().bold(),
            header = format!(": duplicate instances of `{}` were found", self.ident).bold(),
            arrow = "-->".blue().bold(),
            indent = " ",
            file = self.file.to_string_lossy(),
            folder = self.folder.to_string_lossy()
        )
    }
}

#[derive(Debug)]
pub struct UnexpectedModError(pub PathBuf);
impl Display for UnexpectedModError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(
            f,
            "\n\
             {error}{header}\n\
             {indent}{arrow} {file}\n\
            ",
            error = "error".red().bold(),
            arrow = "-->".blue().bold(),
            indent = " ",
            file = self.0.to_string_lossy(),
            header = ": a file cannot be named mod.rhdl unless it is a module".bold(),
        )
    }
}

#[derive(Debug)]
pub struct DirectoryError(pub PathBuf);
impl Display for DirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(
            f,
            "\n\
             {error}{header}\n\
             {indent}{arrow} {file}\n\
            ",
            error = "error".red().bold(),
            arrow = "-->".blue().bold(),
            header = format!(": {}", "cannot compile a directory").bold(),
            indent = " ",
            file = self.0.to_string_lossy(),
        )
    }
}

#[derive(Debug)]
pub struct NotFoundError {
    pub ident: syn::Ident,
    pub file: PathBuf,
    pub folder: PathBuf,
}
impl Display for NotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(
            f,
            "\n\
             {error}{header}\n\
             {indent}{arrow} {file}\n\
             {indent}{arrow} {folder}\n\
            ",
            error = "error".red().bold(),
            header = format!(": could not find a file for `{}` at either of", self.ident).bold(),
            arrow = "-->".blue().bold(),
            indent = " ",
            file = self.file.to_string_lossy(),
            folder = self.folder.to_string_lossy()
        )
    }
}

#[derive(Debug)]
pub struct WrappedIoError {
    pub cause: std::io::Error,
    pub path: PathBuf,
}
impl Display for WrappedIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        writeln!(
            f,
            "{error}{header}",
            error = "error".red().bold(),
            header = format!(
                ": couldn't read {}: {}",
                self.path.to_string_lossy(),
                self.cause
            )
            .bold(),
        )
    }
}

error!(ResolveError {
    IoError => WrappedIoError,
    ParseError => PreciseSynParseError,
    NotFoundError => NotFoundError,
    DuplicateError => DuplicateError,
    UnexpectedModError => UnexpectedModError,
    DirectoryError => DirectoryError,
});

#[derive(Debug)]
pub struct MultipleDefinitionError {
    pub file: crate::resolve::File,
    pub name: syn::Ident,
    pub original: Span,
    pub duplicates: Vec<Span>,
}

impl Display for MultipleDefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        for duplicate in &self.duplicates {
            render_location(
                f,
                format!("the name `{}` is defined multiple times", self.name),
                (Reference::Error, "", self.original),
                vec![(Reference::Info, "", *duplicate)],
                &self.file.path,
                &self.file.content,
            )?;
        }
        Ok(())
    }
}

error!(ScopeError {
    MultipleDefinitionError => MultipleDefinitionError,
});
