//! Parsing tool output into diagnostics.
//!
//! Recognised formats:
//! - Verilator: `%Error: rtl/a.sv:12:5: msg`, `%Warning-WIDTH: rtl/a.sv:12:5: msg`
//! - Yosys:     `rtl/a.sv:12: ERROR: msg`, `rtl/a.sv:12.5-12.9: Warning: msg`
//! - GCC-style (Icarus and others): `rtl/a.sv:12: error: msg`, `a.sv:12:5: warning: msg`
//! - Icarus bare form: `rtl/a.sv:12: syntax error`
//! - Xilinx ISE:  `ERROR:ConstraintSystem:59 - Constraint <...> [top.ucf(12)]: msg`

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub col: Option<u32>,
    pub severity: Severity,
    /// Tool-specific code, e.g. Verilator's `WIDTH`.
    pub code: Option<String>,
    pub message: String,
}

impl Diagnostic {
    /// Make a relative file path absolute against the directory the tool ran in.
    pub fn resolve(mut self, cwd: &Path) -> Self {
        if self.file.is_relative() {
            self.file = cwd.join(&self.file);
        }
        self
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        write!(f, "{}:{}", self.file.display(), self.line)?;
        if let Some(c) = self.col {
            write!(f, ":{c}")?;
        }
        write!(f, ": {sev}")?;
        if let Some(code) = &self.code {
            write!(f, "[{code}]")?;
        }
        write!(f, ": {}", self.message)
    }
}

static ISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(ERROR|WARNING):([A-Za-z]+(?::\d+)?) - (.*?)\[([^\]\(\s]+)\((\d+)\)\]:?\s*(.*)$")
        .unwrap()
});
static VERILATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^%(Error|Warning)(?:-([A-Z0-9_]+))?: ([^:\s][^:]*):(\d+):(?:(\d+):)? ?(.*)$")
        .unwrap()
});
static YOSYS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([^:\s][^:]*):(\d+)(?:\.(\d+)(?:-\d+\.\d+)?)?: (ERROR|Warning|WARNING): (.*)$")
        .unwrap()
});
static GCC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^([^:\s][^:]*):(\d+):(?:(\d+):)?\s*(?:fatal )?(error|warning|sorry|note)\s*:\s*(.*)$",
    )
    .unwrap()
});
static ICARUS_BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^:\s][^:]*\.s?vh?):(\d+):\s*(.*)$").unwrap());

fn num(s: Option<regex::Match<'_>>) -> Option<u32> {
    s.and_then(|m| m.as_str().parse().ok())
}

/// Parse a tool's whole output. Like [`parse_line`], but first joins the
/// indented continuation lines Xilinx tools use to wrap long messages onto the
/// `ERROR:`/`WARNING:` line they belong to.
pub fn parse_lines<S: AsRef<str>>(lines: &[S]) -> Vec<Diagnostic> {
    let mut joined: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_xilinx_msg = false;
    for l in lines {
        let l = l.as_ref();
        if in_xilinx_msg && l.starts_with("   ") && !l.trim().is_empty() {
            let last = joined
                .last_mut()
                .expect("in_xilinx_msg implies a previous line");
            last.push(' ');
            last.push_str(l.trim());
            continue;
        }
        in_xilinx_msg = l.starts_with("ERROR:") || l.starts_with("WARNING:");
        joined.push(l.to_string());
    }
    joined.iter().filter_map(|l| parse_line(l)).collect()
}

/// Parse one output line. Returns `None` for lines that aren't diagnostics.
pub fn parse_line(line: &str) -> Option<Diagnostic> {
    let line = line.trim_end();
    if let Some(c) = ISE.captures(line) {
        let message = format!("{} {}", c[3].trim(), c[6].trim())
            .trim()
            .to_string();
        return Some(Diagnostic {
            file: c[4].into(),
            line: num(c.get(5))?,
            col: None,
            severity: if &c[1] == "ERROR" {
                Severity::Error
            } else {
                Severity::Warning
            },
            code: Some(c[2].to_string()),
            message,
        });
    }
    if let Some(c) = VERILATOR.captures(line) {
        return Some(Diagnostic {
            file: c[3].into(),
            line: num(c.get(4))?,
            col: num(c.get(5)),
            severity: if &c[1] == "Error" {
                Severity::Error
            } else {
                Severity::Warning
            },
            code: c.get(2).map(|m| m.as_str().to_string()),
            message: c[6].trim().to_string(),
        });
    }
    if let Some(c) = YOSYS.captures(line) {
        return Some(Diagnostic {
            file: c[1].into(),
            line: num(c.get(2))?,
            col: num(c.get(3)),
            severity: if &c[4] == "ERROR" {
                Severity::Error
            } else {
                Severity::Warning
            },
            code: None,
            message: c[5].trim().to_string(),
        });
    }
    if let Some(c) = GCC.captures(line) {
        let severity = match &c[4] {
            "warning" => Severity::Warning,
            "note" => Severity::Note,
            // Icarus "sorry" means unsupported, except this one, which only
            // widens an always_* block's sensitivity and simulates correctly.
            "sorry" if c[5].contains("will be sensitive to all bits") => Severity::Warning,
            _ => Severity::Error, // error, sorry (= unsupported)
        };
        return Some(Diagnostic {
            file: c[1].into(),
            line: num(c.get(2))?,
            col: num(c.get(3)),
            severity,
            code: None,
            message: c[5].trim().to_string(),
        });
    }
    if let Some(c) = ICARUS_BARE.captures(line) {
        let message = c[3].trim().to_string();
        // The bare form carries errors ("syntax error", "I give up.") but also
        // context and runtime notes ("a.sv:3:      : The port is declared here.",
        // "tb.sv:41: $finish called at 351000 (1ps)").
        let lower = message.to_lowercase();
        let severity = if !message.starts_with(':')
            && (lower.contains("error") || lower.contains("give up"))
        {
            Severity::Error
        } else {
            Severity::Note
        };
        return Some(Diagnostic {
            file: c[1].into(),
            line: num(c.get(2))?,
            col: None,
            severity,
            code: None,
            message: message.trim_start_matches(':').trim().to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(line: &str) -> Diagnostic {
        parse_line(line).unwrap_or_else(|| panic!("no diagnostic in {line:?}"))
    }

    #[test]
    fn verilator_error_and_warning() {
        let e = d("%Error: rtl/a.sv:12:5: syntax error, unexpected IDENTIFIER");
        assert_eq!(
            (e.file.to_str().unwrap(), e.line, e.col),
            ("rtl/a.sv", 12, Some(5))
        );
        assert_eq!(e.severity, Severity::Error);
        assert_eq!(e.message, "syntax error, unexpected IDENTIFIER");
        let w = d("%Warning-WIDTHTRUNC: /p/rtl/a.sv:3:15: Operator ASSIGN expects 4 bits");
        assert_eq!(w.severity, Severity::Warning);
        assert_eq!(w.code.as_deref(), Some("WIDTHTRUNC"));
        assert_eq!(w.col, Some(15));
    }

    #[test]
    fn gcc_fatal_error() {
        let e = d("/x/fstcpp_writer.cpp:18:10: fatal error: lz4.h: No such file or directory");
        assert_eq!((e.line, e.col, e.severity), (18, Some(10), Severity::Error));
        assert_eq!(e.message, "lz4.h: No such file or directory");
    }

    #[test]
    fn ise_constraint_error() {
        // Real ngdbuild output for a mistyped net name.
        let e = d(
            r#"ERROR:ConstraintSystem:59 - Constraint <NET "ledd" LOC = "R20" |> [bad.ucf(12)]: NET "ledd" not found."#,
        );
        assert_eq!(
            (e.file.to_str().unwrap(), e.line, e.severity),
            ("bad.ucf", 12, Severity::Error)
        );
        assert_eq!(e.code.as_deref(), Some("ConstraintSystem:59"));
        assert!(e.message.starts_with("Constraint <NET"));
        assert!(parse_line(
            "ERROR:Security:9c - No 'ISE' nor 'WebPack' feature version 2013.10 was available"
        )
        .is_none());
    }

    #[test]
    fn ise_wrapped_message_is_joined() {
        // Real ngdbuild output: the file reference is on the continuation line.
        let lines = [
            r#"ERROR:ConstraintSystem:59 - Constraint <NET "ghost" LOC = "A2" ;>"#,
            r#"   [blinky.ucf(16)]: NET "ghost" not found.  Please verify that:"#,
            r#"   1. The specified design element actually exists in the original design."#,
            "",
            "rtl/a.sv:3: error: unrelated",
        ];
        let d = parse_lines(&lines);
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].file.to_str().unwrap(), d[0].line), ("blinky.ucf", 16));
        assert!(d[0].message.contains("not found"));
        assert_eq!(d[1].line, 3);
    }

    #[test]
    fn verilator_summary_lines_are_ignored() {
        assert!(parse_line("%Error: Exiting due to 1 error(s)").is_none());
    }

    #[test]
    fn yosys_formats() {
        let e = d("rtl/a.sv:7: ERROR: syntax error, unexpected TOK_ID");
        assert_eq!((e.line, e.col, e.severity), (7, None, Severity::Error));
        let w = d("rtl/a.sv:9.3-9.12: Warning: Identifier `x' is implicitly declared.");
        assert_eq!((w.line, w.col, w.severity), (9, Some(3), Severity::Warning));
    }

    #[test]
    fn icarus_formats() {
        let e = d("tb/tb.sv:20: error: Unknown module type: blinkyy");
        assert_eq!((e.line, e.severity), (20, Severity::Error));
        assert_eq!(e.message, "Unknown module type: blinkyy");
        let s = d("rtl/a.sv:4: syntax error");
        assert_eq!(s.severity, Severity::Error);
        let w = d("rtl/a.sv:5: warning: Port 2 (led) of blinky is unconnected.");
        assert_eq!(w.severity, Severity::Warning);
        let f = d("/p/tb/tb.sv:41: $finish called at 351000 (1ps)");
        assert_eq!(f.severity, Severity::Note);
        let n = d("rtl/a.sv:3:      : The port is declared here.");
        assert_eq!(n.severity, Severity::Note);
        assert_eq!(n.message, "The port is declared here.");
        let wide = d("rtl/s.sv:94: sorry: constant selects in always_* processes are not fully supported (the process will be sensitive to all bits in 'ph[31:0]').");
        assert_eq!(wide.severity, Severity::Warning);
        let unsup = d("rtl/s.sv:9: sorry: Unsupported dynamic array method.");
        assert_eq!(unsup.severity, Severity::Error);
    }

    #[test]
    fn ordinary_output_is_not_a_diagnostic() {
        for l in [
            "PASS: tb_blinky",
            "VCD info: dumpfile tb.vcd opened for output.",
            "",
            "Number of cells: 12",
        ] {
            assert!(parse_line(l).is_none(), "{l:?}");
        }
    }

    #[test]
    fn resolve_and_display() {
        let e = d("rtl/a.sv:7:2: error: bad").resolve(Path::new("/proj"));
        assert_eq!(e.to_string(), "/proj/rtl/a.sv:7:2: error: bad");
        let abs = d("/x/a.sv:1: error: y").resolve(Path::new("/proj"));
        assert_eq!(abs.file, PathBuf::from("/x/a.sv"));
    }
}
