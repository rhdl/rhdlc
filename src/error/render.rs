use std::ffi::OsStr;
use std::fmt;
use std::fmt::Display;
use std::path::Path;

use colored::Colorize;
use proc_macro2::Span;

use crate::resolve::ResolutionSource;

const MOD_FILE_STEM: &str = "mod";
const UNKNOWN_FILE: &str = "???.rhdl";
const UNKNOWN_DIRECTORY: &str = "???";

const ARROW: &str = "-->";
const PIPE: &str = "|";

pub enum Reference {
    Error,
    Warning,
    Info,
}

pub fn render_location<'a, C>(
    f: &mut fmt::Formatter,
    cause: C,
    main_reference: (Reference, &'a str, Span),
    mut references: Vec<(Reference, &'a str, Span)>,
    source: &ResolutionSource,
    code: &str,
) -> fmt::Result
where
    C: Display,
{
    let filepath = match source {
        ResolutionSource::File(path) => {
            let filename = path
                .file_name()
                .map(OsStr::to_string_lossy)
                .unwrap_or_else(|| UNKNOWN_FILE.into());

            if path
                .file_stem()
                .map(OsStr::to_string_lossy)
                .map(|stem| stem == MOD_FILE_STEM)
                .unwrap_or(false)
            {
                path.parent()
                    .and_then(Path::file_stem)
                    .map(OsStr::to_string_lossy)
                    .unwrap_or_else(|| UNKNOWN_DIRECTORY.into())
                    + "/"
                    + filename
            } else {
                filename
            }
        }
        ResolutionSource::Stdin => "<stdin>".into(),
    };

    let main_max_line = main_reference.2.start().max(main_reference.2.end());
    let max_line = references
        .iter()
        .map(|(_, _, span)| span.start().max(span.end()))
        .max()
        .map(|max_ref| max_ref.max(main_max_line))
        .unwrap_or(main_max_line);
    let indent = " ".repeat(max_line.line.to_string().len());

    use Reference::*;
    let msg = match main_reference.0 {
        Error => {
            write!(f, "{}", "error".red().bold())?;
            main_reference.1.red().bold()
        }
        Warning => {
            write!(f, "{}", "warning".yellow().bold())?;
            main_reference.1.yellow().bold()
        }
        Info => {
            write!(f, "{}", "info".blue().bold())?;
            main_reference.1.blue().bold()
        }
    };

    writeln!(f, "{}", format!(": {}", cause).bold())?;
    write!(
        f,
        "{indent}{arrow} {filepath}",
        indent = indent,
        arrow = ARROW.blue().bold(),
        filepath = filepath
    )?;

    let start = main_reference.2.start();
    let end = main_reference.2.end();
    // Fallback render
    if start.line == end.line && start.column == end.column {
        // Need an extra line here
        writeln!(f)?;
        writeln!(
            f,
            "{indent} {pipe} {msg}",
            indent = indent,
            pipe = PIPE.blue().bold(),
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
        writeln!(
            f,
            "{indent} {pipe}",
            indent = indent,
            pipe = PIPE.blue().bold()
        )?;
    }

    references.sort_by(|a, b| a.2.start().cmp(&b.2.start()));

    for r in references.iter().filter(|r| r.2.start() <= start) {
        write_ref(f, r, &indent, code)?;
    }

    write_ref(f, &main_reference, &indent, code)?;

    for r in references.iter().filter(|r| r.2.start() > start) {
        write_ref(f, r, &indent, code)?;
    }

    Ok(())
}

/// Display a reference with its code
/// TODO: support multi-line references
fn write_ref(
    f: &mut fmt::Formatter,
    r: &(Reference, &str, Span),
    indent: &str,
    code: &str,
) -> fmt::Result {
    use Reference::*;

    let (start, mut end) = (r.2.start(), r.2.end());
    let code_line = match code.lines().nth(start.line - 1) {
        Some(line) => line,
        None => return Err(fmt::Error),
    };
    if end.line > start.line {
        end.line = start.line;
        end.column = code_line.len();
    }

    let label = start.line.to_string().blue().bold();

    writeln!(
        f,
        "{label_indent}{label} {pipe} {code}",
        label_indent = " ".repeat(indent.len() - label.len()),
        label = label,
        pipe = PIPE.blue().bold(),
        code = code_line.trim_end()
    )?;

    let underline = match r.0 {
        Error => "^".repeat(end.column - start.column).red().bold(),
        Info => "-".repeat(end.column - start.column).blue().bold(),
        Warning => "^".repeat(end.column - start.column).yellow().bold(),
    };

    let msg = match r.0 {
        Error => r.1.red().bold(),
        Info => r.1.blue().bold(),
        Warning => r.1.blue().bold(),
    };

    writeln!(
        f,
        "{indent} {pipe} {offset}{underline} {msg}",
        indent = indent,
        pipe = PIPE.blue().bold(),
        offset = " ".repeat(start.column),
        underline = underline,
        msg = msg
    )
}
