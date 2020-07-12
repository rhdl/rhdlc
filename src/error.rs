use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::fmt::Display;
use std::path::{Path, PathBuf};

use colored::Colorize;
use log::debug;

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
        Self::render_location(f, &self.cause, &self.path, &self.code)
    }
}

impl PreciseSynParseError {
    const MOD_FILENAME: &'static str = "mod.rhdl";

    fn render_fallback(
        formatter: &mut fmt::Formatter,
        cause: &syn::Error,
        filepath: &str,
    ) -> fmt::Result {
        debug!("falling back");
        write!(
            formatter,
            "\n\
             {error}{header}\n\
             {indent}{arrow} {filepath}\n\
             {indent} {pipe} {message}\n\
             ",
            error = "error".red().bold(),
            header = format!(": {}", cause).bold(),
            indent = " ",
            arrow = "-->".blue().bold(),
            filepath = filepath,
            pipe = "|".blue().bold(),
            message = cause.to_string().red().bold(),
        )
    }

    /// Based off of https://github.com/dtolnay/syn/blob/master/examples/dump-syntax/src/main.rs#L94
    /// to render a rustc-style message, including colors.
    fn render_location(
        formatter: &mut fmt::Formatter,
        cause: &syn::Error,
        path: &Path,
        code: &str,
    ) -> fmt::Result {
        let filename = path
            .file_name()
            .map(OsStr::to_string_lossy)
            .unwrap_or(Cow::Borrowed("Unknown File"));

        let filepath = if filename == Self::MOD_FILENAME {
            path.parent()
                .and_then(Path::file_name)
                .map(OsStr::to_string_lossy)
                .unwrap_or(Cow::Borrowed("Unknown Directory"))
                + "/"
                + filename
        } else {
            filename
        };

        let start = cause.span().start();
        let mut end = cause.span().end();
        if start.line == end.line && start.column == end.column {
            return Self::render_fallback(formatter, cause, &filepath);
        }
        let code_line = match code.lines().nth(start.line - 1) {
            Some(line) => line,
            None => return Self::render_fallback(formatter, cause, &filepath),
        };
        if end.line > start.line {
            end.line = start.line;
            end.column = code_line.len();
        }

        write!(
            formatter,
            "\n\
             {error}{header}\n\
             {indent}{arrow} {filepath}:{linenum}:{colnum}\n\
             {indent} {pipe}\n\
             {label} {pipe} {code}\n\
             {indent} {pipe} {offset}{underline} {message}\n\
             ",
            error = "error".red().bold(),
            header = format!(": {}", cause).bold(),
            indent = " ".repeat(start.line.to_string().len()),
            arrow = "-->".blue().bold(),
            filepath = filepath,
            linenum = start.line,
            colnum = start.column,
            pipe = "|".blue().bold(),
            label = start.line.to_string().blue().bold(),
            code = code_line.trim_end(),
            offset = " ".repeat(start.column),
            underline = "^".repeat(end.column - start.column).red().bold(),
            message = cause.to_string().red().bold(),
        )
    }
}

error!(ResolveError {
    IoError => std::io::Error,
    ParseError => PreciseSynParseError,
    NotFoundError => syn::Ident,
});
