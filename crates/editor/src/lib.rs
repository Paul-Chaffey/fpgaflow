//! Opens files in the user's chosen external editor.
//!
//! Every editable file in a project goes through here: RTL, testbenches,
//! `fpgaflow.toml`, constraint files, Yosys script overrides and the user
//! config, so they all open in the same editor.

use std::path::Path;
use std::process::{Command, Stdio};

use fpgaflow_core::{EditorConfig, EditorPreset};

/// A 1-based position in a file, as compilers report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub col: Option<u32>,
}

impl Position {
    pub fn line(line: u32) -> Self {
        Self { line, col: None }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("editor preset is \"custom\" but no `command` is set in the config")]
    NoCustomCommand,
    #[error("cannot parse editor command {template:?}: {source}")]
    Parse {
        template: String,
        #[source]
        source: shell_words::ParseError,
    },
    #[error("editor command is empty")]
    Empty,
    #[error("cannot start editor `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// (template with position, template without position)
fn templates(cfg: &EditorConfig) -> Result<(String, String), EditorError> {
    let pair = |a: &str, b: &str| Ok((a.to_string(), b.to_string()));
    match cfg.preset {
        EditorPreset::Kate => pair("kate --line {line} --column {col} {file}", "kate {file}"),
        EditorPreset::Kwrite => pair(
            "kwrite --line {line} --column {col} {file}",
            "kwrite {file}",
        ),
        EditorPreset::Vscode => pair("code --goto {file}:{line}:{col}", "code {file}"),
        EditorPreset::Gvim => pair(
            "gvim --remote-silent +{line} {file}",
            "gvim --remote-silent {file}",
        ),
        EditorPreset::Emacsclient => pair(
            "emacsclient -n +{line}:{col} {file}",
            "emacsclient -n {file}",
        ),
        EditorPreset::Custom => {
            let with = cfg.command.clone().ok_or(EditorError::NoCustomCommand)?;
            let without = cfg
                .command_no_position
                .clone()
                .unwrap_or_else(|| strip_position(&with));
            Ok((with, without))
        }
    }
}

/// Derive a no-position template from a custom one by dropping every word
/// that mentions `{line}` or `{col}`, plus a preceding option flag
/// (`--line {line}` or `-l {line}`).
fn strip_position(template: &str) -> String {
    let words = shell_words::split(template).unwrap_or_else(|_| vec![template.to_string()]);
    let mut out: Vec<String> = Vec::new();
    for w in words {
        if w.contains("{line}") || w.contains("{col}") {
            // A bare placeholder is the value of the flag before it; something
            // like `+{line}` stands alone and leaves the previous word alone.
            let is_flag_value = w == "{line}" || w == "{col}";
            if is_flag_value && out.last().is_some_and(|p| p.starts_with('-')) {
                out.pop();
            }
        } else {
            out.push(w);
        }
    }
    shell_words::join(out)
}

/// Build the argv that opens `file` (optionally at `pos`).
///
/// The template is split into words *before* substitution, so file names
/// containing spaces stay a single argument.
pub fn argv(
    cfg: &EditorConfig,
    file: &Path,
    pos: Option<Position>,
) -> Result<Vec<String>, EditorError> {
    let (with, without) = templates(cfg)?;
    let template = if pos.is_some() { with } else { without };
    let words = shell_words::split(&template).map_err(|source| EditorError::Parse {
        template: template.clone(),
        source,
    })?;
    if words.is_empty() {
        return Err(EditorError::Empty);
    }
    let file = file.to_string_lossy();
    let (line, col) = match pos {
        Some(p) => (p.line.to_string(), p.col.unwrap_or(1).to_string()),
        None => (String::new(), String::new()),
    };
    Ok(words
        .into_iter()
        .map(|w| {
            w.replace("{file}", &file)
                .replace("{line}", &line)
                .replace("{col}", &col)
        })
        .collect())
}

/// Open `file` in the configured editor without waiting for it to close.
///
/// The editor runs in its own process group with no stdio, so closing
/// fpgaflow does not close the editor.
pub fn open(cfg: &EditorConfig, file: &Path, pos: Option<Position>) -> Result<(), EditorError> {
    use std::os::unix::process::CommandExt;
    let argv = argv(cfg, file, pos)?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|source| EditorError::Spawn {
            program: argv[0].clone(),
            source,
        })?;
    // Reap it when it exits so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(preset: EditorPreset) -> EditorConfig {
        EditorConfig {
            preset,
            ..Default::default()
        }
    }

    #[test]
    fn kate_with_and_without_position() {
        let f = Path::new("/p/rtl/my top.sv");
        assert_eq!(
            argv(
                &cfg(EditorPreset::Kate),
                f,
                Some(Position {
                    line: 12,
                    col: Some(5)
                })
            )
            .unwrap(),
            ["kate", "--line", "12", "--column", "5", "/p/rtl/my top.sv"]
        );
        assert_eq!(
            argv(&cfg(EditorPreset::Kate), f, None).unwrap(),
            ["kate", "/p/rtl/my top.sv"]
        );
    }

    #[test]
    fn missing_column_defaults_to_one() {
        let v = argv(
            &cfg(EditorPreset::Vscode),
            Path::new("a.v"),
            Some(Position::line(3)),
        )
        .unwrap();
        assert_eq!(v, ["code", "--goto", "a.v:3:1"]);
    }

    #[test]
    fn custom_command_and_derived_no_position_form() {
        let c = EditorConfig {
            preset: EditorPreset::Custom,
            command: Some("myedit --line {line} -c {col} --wait {file}".into()),
            command_no_position: None,
        };
        assert_eq!(
            argv(
                &c,
                Path::new("x.sv"),
                Some(Position {
                    line: 7,
                    col: Some(2)
                })
            )
            .unwrap(),
            ["myedit", "--line", "7", "-c", "2", "--wait", "x.sv"]
        );
        assert_eq!(
            argv(&c, Path::new("x.sv"), None).unwrap(),
            ["myedit", "--wait", "x.sv"]
        );
    }

    #[test]
    fn attached_placeholder_keeps_previous_flag() {
        let words =
            shell_words::split(&strip_position("gvim --remote-silent +{line} {file}")).unwrap();
        assert_eq!(words, ["gvim", "--remote-silent", "{file}"]);
    }

    #[test]
    fn custom_without_command_is_an_error() {
        let c = cfg(EditorPreset::Custom);
        assert!(matches!(
            argv(&c, Path::new("x"), None),
            Err(EditorError::NoCustomCommand)
        ));
    }
}
