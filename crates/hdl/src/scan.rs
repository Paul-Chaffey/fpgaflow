//! Lightweight source scanning.
//!
//! This is deliberately a heuristic, not a parser. It is used where a
//! best-effort answer is fine and running a tool would be overkill: finding
//! the testbench top when none is configured, and building a first GTKWave
//! layout. Anything that must be right (synthesis hierarchy, ports) comes
//! from Yosys via [`crate::design`].

use std::sync::LazyLock;

use regex::Regex;

/// Remove `//` and `/* */` comments, leaving string literals intact.
/// Newlines are kept so line numbers still match the original.
pub fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push(c);
                while let Some(s) = chars.next() {
                    out.push(s);
                    if s == '\\' {
                        if let Some(e) = chars.next() {
                            out.push(e);
                        }
                    } else if s == '"' || s == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for s in chars.by_ref() {
                    if s == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for s in chars.by_ref() {
                    if s == '\n' {
                        out.push('\n');
                    }
                    if prev == '*' && s == '/' {
                        break;
                    }
                    prev = s;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

static MODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:module|macromodule)\s+(?:(?:automatic|static)\s+)?([A-Za-z_][A-Za-z0-9_$]*)")
        .unwrap()
});

/// Names of all modules declared in `src`, in order.
pub fn declared_modules(src: &str) -> Vec<String> {
    let clean = strip_comments(src);
    MODULE
        .captures_iter(&clean)
        .map(|c| c[1].to_string())
        .collect()
}

/// A signal declared at the top level of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub name: String,
    /// `Some((msb, lsb))` for a vector with a numeric range, `None` for a scalar.
    pub range: Option<(i64, i64)>,
}

static END_MODULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bendmodule\b").unwrap());
static NESTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\b(task|function)\b.*?\bend(?:task|function)\b").unwrap());
static DECL_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:(?:input|output|inout)\s+)?(?:var\s+)?(?:logic|reg|wire|bit)\s+(?:(?:signed|unsigned)\s+)?(?:\[\s*([^\]:]+?)\s*:\s*([^\]]+?)\s*\]\s*)?([A-Za-z_]\w*)",
    )
    .unwrap()
});
/// `, name` continuing a declaration list, optionally after an initialiser.
static DECL_MORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:=\s*[^,;()]*)?\s*,\s*([A-Za-z_]\w*)").unwrap());

const KEYWORDS: &[&str] = &[
    "input",
    "output",
    "inout",
    "logic",
    "reg",
    "wire",
    "bit",
    "var",
    "signed",
    "unsigned",
    "int",
    "integer",
    "string",
    "parameter",
    "localparam",
    "real",
    "time",
];

/// Signals declared at the top level of module `module` in `src`, for a
/// default waveform layout. Vectors whose range is not numeric (e.g. uses a
/// parameter) are skipped, since GTKWave save files need the exact range.
pub fn module_signals(src: &str, module: &str) -> Vec<Signal> {
    let clean = strip_comments(src);
    let start = MODULE
        .captures_iter(&clean)
        .find(|c| &c[1] == module)
        .map(|c| c.get(0).unwrap().end());
    let Some(start) = start else {
        return Vec::new();
    };
    let body = &clean[start..];
    let body = END_MODULE.find(body).map_or(body, |m| &body[..m.start()]);
    let body = NESTED.replace_all(body, " ");

    let mut out: Vec<Signal> = Vec::new();
    let mut push = |name: &str, range| {
        if !out.iter().any(|s: &Signal| s.name == name) {
            out.push(Signal {
                name: name.to_string(),
                range,
            });
        }
    };
    for c in DECL_HEAD.captures_iter(&body) {
        let range = match (c.get(1), c.get(2)) {
            (Some(m), Some(l)) => match (m.as_str().trim().parse(), l.as_str().trim().parse()) {
                (Ok(m), Ok(l)) => Some(Some((m, l))),
                _ => None, // non-numeric range: skip this declaration
            },
            _ => Some(None),
        };
        let Some(range) = range else { continue };
        push(&c[3], range);
        let mut rest = &body[c.get(0).unwrap().end()..];
        while let Some(m) = DECL_MORE.captures(rest) {
            let name = m.get(1).unwrap();
            if KEYWORDS.contains(&name.as_str()) {
                break;
            }
            push(name.as_str(), range);
            rest = &rest[m.get(0).unwrap().end()..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TB: &str = r#"
`timescale 1ns/1ps
// module fake_in_comment;
module tb_blinky;
    localparam int unsigned WIDTH = 4;
    logic clk = 1'b0;
    logic rst = 1'b1;
    logic led;
    logic [7:0] data, other;
    logic [WIDTH-1:0] skipped;
    int   errors = 0;
    /* module also_fake; */
    blinky #(.WIDTH(WIDTH)) dut (.clk(clk), .rst(rst), .led(led));
    initial $display("// not a comment");
    task automatic expect_led(input logic value, input string when);
        logic inner;
    endtask
endmodule

module helper(input wire a, output reg [3:0] q);
endmodule
"#;

    #[test]
    fn finds_modules_but_not_in_comments() {
        assert_eq!(declared_modules(TB), ["tb_blinky", "helper"]);
    }

    #[test]
    fn strip_keeps_strings_and_lines() {
        let s = strip_comments("a // x\n\"b // y\" /* z\n */ c");
        assert_eq!(s, "a \n\"b // y\" \n  c");
    }

    #[test]
    fn top_level_signals() {
        let sigs = module_signals(TB, "tb_blinky");
        let names: Vec<_> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["clk", "rst", "led", "data", "other"]);
        assert_eq!(sigs[3].range, Some((7, 0)));
        assert_eq!(sigs[0].range, None);
    }

    #[test]
    fn ansi_ports() {
        let sigs = module_signals(TB, "helper");
        assert_eq!(
            sigs,
            [
                Signal {
                    name: "a".into(),
                    range: None
                },
                Signal {
                    name: "q".into(),
                    range: Some((3, 0))
                }
            ]
        );
    }
}
