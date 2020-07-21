use std::ffi::OsStr;
use std::fmt;
use std::fmt::Display;
use std::path::Path;

use colored::Colorize;
use proc_macro2::Span;

const MOD_FILE_STEM: &'static str = "mod";
const UNKNOWN_FILE: &'static str = "???.rhdl";
const UNKNOWN_DIRECTORY: &'static str = "???";

pub enum Routine {
    Error,
    Warning,
}

pub fn render_location<'a, C>(
    f: &mut fmt::Formatter,
    cause: C,
    routine: (Routine, &'a str, Span),
    mut references: Vec<(&'a str, Span)>,
    path: &Path,
    code: &str,
) -> fmt::Result
where
    C: Display,
{
    let filename = path
        .file_name()
        .map(OsStr::to_string_lossy)
        .unwrap_or(UNKNOWN_FILE.into());

    let filepath = if path
        .file_stem()
        .map(OsStr::to_string_lossy)
        .map(|stem| stem == MOD_FILE_STEM)
        .unwrap_or(false)
    {
        path.parent()
            .and_then(Path::file_stem)
            .map(OsStr::to_string_lossy)
            .unwrap_or(UNKNOWN_DIRECTORY.into())
            + "/"
            + filename
    } else {
        filename
    };

    use Routine::*;
    let msg = match routine.0 {
        Error => {
            writeln!(f)?;
            write!(f, "{}", "error".red().bold())?;
            routine.1.red().bold()
        }
        Warning => {
            writeln!(f)?;
            write!(f, "{}", "warning".yellow().bold())?;
            routine.1.yellow().bold()
        }
    };
    writeln!(f, "{}", format!(": {}", cause).bold())?;
    let indent = " ";
    let arrow = "-->".blue().bold();
    let pipe = "|".blue().bold();
    write!(
        f,
        "{indent}{arrow} {filepath}",
        indent = indent,
        arrow = arrow,
        filepath = filepath
    )?;

    let start = routine.2.start();
    let mut end = routine.2.end();
    if start.line == end.line && start.column == end.column {
        // Fallback render
        writeln!(
            f,
            "{indent} {pipe} {msg}",
            indent = indent,
            pipe = pipe,
            msg = msg
        )?;
        return Ok(());
    } else {
        writeln!(
            f,
            ":{linenum}:{colnum}",
            linenum = start.line,
            colnum = start.column
        )?;
        writeln!(f, "{indent} {pipe}", indent = indent, pipe = pipe)?;
    }
    references.sort_by(|a, b| a.1.start().cmp(&b.1.start()));

    for r in references.iter().filter(|r| r.1.start() < start) {
        write_ref(f, r, code)?;
    }

    {
        let code_line = match code.lines().nth(start.line - 1) {
            Some(line) => line,
            None => return Err(fmt::Error),
        };
        if end.line > start.line {
            end.line = start.line;
            end.column = code_line.len();
        }
        let underline = "^".repeat(end.column - start.column);
        let underline = match routine.0 {
            Error => underline.red().bold(),
            Warning => underline.yellow().bold(),
        };
        writeln!(
            f,
            "\n\
         {label} {pipe} {code}\n\
         {indent} {pipe} {offset}{underline} {msg}",
            indent = " ".repeat(start.line.to_string().len()),
            pipe = "|".blue().bold(),
            label = start.line.to_string().blue().bold(),
            code = code_line.trim_end(),
            offset = " ".repeat(start.column),
            underline = underline,
            msg = msg,
        )?;
    }

    for r in references.iter().filter(|r| r.1.start() > start) {
        write_ref(f, r, code)?;
    }

    Ok(())
}

fn write_ref(f: &mut fmt::Formatter, r: &(&str, Span), code: &str) -> fmt::Result {
    let (start, mut end) = (r.1.start(), r.1.end());
    let code_line = match code.lines().nth(start.line - 1) {
        Some(line) => line,
        None => return Err(fmt::Error),
    };
    if end.line > start.line {
        end.line = start.line;
        end.column = code_line.len();
    }
    let pipe = "|".blue().bold();
    writeln!(
        f,
        "{label} {pipe} {code}",
        label = start.line.to_string().blue().bold(),
        pipe = pipe,
        code = code_line.trim_end()
    )?;
    writeln!(
        f,
        "{indent} {pipe} {offset}{underline} {msg}",
        indent = " ".repeat(start.line.to_string().len()),
        pipe = pipe,
        offset = " ".repeat(start.column),
        underline = "-".repeat(end.column - start.column).blue().bold(),
        msg = r.0.to_string().blue().bold()
    )
}
