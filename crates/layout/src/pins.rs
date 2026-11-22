//! Pin assignments: read them from the project's UCFs, check them against
//! the package and the board, and edit them in place.
//!
//! Edits keep the user's files intact: an existing `NET "x" LOC = ...` line
//! is changed where it is; a new assignment goes into a marked section at
//! the end of the project's first UCF; clearing comments the line out.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fpgaflow_core::board::Board;
use fpgaflow_pnr::constraints::ucf_pin_locs;
use regex::Regex;

use crate::package::Package;

/// Header of the section the pin planner appends to.
pub const SECTION: &str = "# ---- fpgaflow pin planner";

/// One bit of a top-level port, named as a UCF names it (`led<0>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortBit {
    pub name: String,
    pub port: String,
    /// `input`, `output` or `inout`.
    pub direction: String,
}

/// Expand ports into their bits: `led` x 8 -> `led<0>` .. `led<7>`.
pub fn port_bits<'a>(ports: impl IntoIterator<Item = (&'a str, usize, &'a str)>) -> Vec<PortBit> {
    let mut out = Vec::new();
    for (name, width, dir) in ports {
        if width <= 1 {
            out.push(PortBit {
                name: name.to_string(),
                port: name.to_string(),
                direction: dir.to_string(),
            });
        } else {
            for i in 0..width {
                out.push(PortBit {
                    name: format!("{name}<{i}>"),
                    port: name.to_string(),
                    direction: dir.to_string(),
                });
            }
        }
    }
    out
}

/// `led[3]`, `led(3)` and `led<3>` all mean `led<3>`.
pub fn normalise(net: &str) -> String {
    net.replace(['[', '('], "<").replace([']', ')'], ">")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// Net as written in the UCF.
    pub net: String,
    pub pin: String,
    pub iostandard: Option<String>,
    pub file: PathBuf,
    /// 1-based line.
    pub line: u32,
}

static IOSTD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)IOSTANDARD\s*=\s*"?([A-Za-z0-9_]+)"?"#).unwrap());
static LOC_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(LOC\s*=\s*)"?[A-Za-z0-9_]+"?"#).unwrap());

/// Pin assignments in the given UCF files (`(path, text)`), in order.
pub fn read_assignments(files: &[(PathBuf, String)]) -> Vec<Assignment> {
    let mut out = Vec::new();
    for (path, text) in files {
        let lines: Vec<&str> = text.lines().collect();
        for l in ucf_pin_locs(text) {
            let line = lines.get(l.line as usize - 1).copied().unwrap_or("");
            let code = line.split('#').next().unwrap_or("");
            out.push(Assignment {
                net: l.net,
                pin: l.pin,
                iostandard: IOSTD.captures(code).map(|c| c[1].to_ascii_uppercase()),
                file: path.clone(),
                line: l.line,
            });
        }
    }
    out
}

/// Read every file in `paths` that exists.
pub fn read_files(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok().map(|t| (p.clone(), t)))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Issue {
    Unassigned {
        port: String,
    },
    NotInPackage {
        net: String,
        pin: String,
    },
    NotUserIo {
        net: String,
        pin: String,
        function: String,
    },
    Reserved {
        net: String,
        pin: String,
        reason: String,
    },
    UsedTwice {
        pin: String,
        nets: Vec<String>,
    },
    Standard {
        net: String,
        pin: String,
        bank: u32,
        expected: String,
        found: String,
    },
    ClockNotGclk {
        net: String,
        pin: String,
    },
    NotAPort {
        net: String,
    },
}

impl Issue {
    pub fn severity(&self) -> Severity {
        match self {
            Issue::Standard { .. } | Issue::ClockNotGclk { .. } | Issue::NotAPort { .. } => {
                Severity::Warning
            }
            _ => Severity::Error,
        }
    }
    /// The pin the issue is about, if any.
    pub fn pin(&self) -> Option<&str> {
        match self {
            Issue::NotInPackage { pin, .. }
            | Issue::NotUserIo { pin, .. }
            | Issue::Reserved { pin, .. }
            | Issue::UsedTwice { pin, .. }
            | Issue::Standard { pin, .. }
            | Issue::ClockNotGclk { pin, .. } => Some(pin),
            _ => None,
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::Unassigned { port } => write!(f, "{port} has no pin"),
            Issue::NotInPackage { net, pin } => {
                write!(f, "{net}: {pin} is not a ball of this package")
            }
            Issue::NotUserIo { net, pin, function } => {
                write!(f, "{net}: {pin} is {function}, not a user I/O pin")
            }
            Issue::Reserved { net, pin, reason } => {
                write!(f, "{net}: {pin} is reserved ({reason})")
            }
            Issue::UsedTwice { pin, nets } => {
                write!(f, "{pin} is assigned to {}", nets.join(" and "))
            }
            Issue::Standard {
                net,
                pin,
                bank,
                expected,
                found,
            } => write!(
                f,
                "{net}: {pin} is in bank {bank} ({expected}) but the UCF says {found}"
            ),
            Issue::ClockNotGclk { net, pin } => {
                write!(
                    f,
                    "{net} looks like a clock but {pin} is not a global clock (GCLK) pin"
                )
            }
            Issue::NotAPort { net } => write!(f, "{net} has a pin but is not a top-level port"),
        }
    }
}

fn looks_like_clock(net: &str) -> bool {
    let n = net.to_ascii_lowercase();
    n == "clk" || n.starts_with("clk_") || n.ends_with("_clk") || n.starts_with("clock")
}

/// Check assignments against the design's ports, the package and the board.
pub fn check(
    ports: &[PortBit],
    assigned: &[Assignment],
    package: &Package,
    board: Option<&Board>,
) -> Vec<Issue> {
    let mut issues = Vec::new();
    let assigned_keys: HashSet<String> = assigned.iter().map(|a| normalise(&a.net)).collect();
    let port_keys: HashSet<String> = ports.iter().map(|p| p.name.clone()).collect();
    for p in ports {
        if !assigned_keys.contains(&p.name) {
            issues.push(Issue::Unassigned {
                port: p.name.clone(),
            });
        }
    }
    let mut by_pin: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for a in assigned {
        let key = normalise(&a.net);
        if !ports.is_empty() && !port_keys.contains(&key) {
            issues.push(Issue::NotAPort { net: a.net.clone() });
        }
        by_pin.entry(&a.pin).or_default().push(&a.net);
        let Some(pp) = package.pin(&a.pin) else {
            issues.push(Issue::NotInPackage {
                net: a.net.clone(),
                pin: a.pin.clone(),
            });
            continue;
        };
        if let Some(reason) = board.and_then(|b| b.reserved_reason(&a.pin)) {
            issues.push(Issue::Reserved {
                net: a.net.clone(),
                pin: a.pin.clone(),
                reason: reason.to_string(),
            });
        } else if !pp.is_user_io() {
            issues.push(Issue::NotUserIo {
                net: a.net.clone(),
                pin: a.pin.clone(),
                function: pp.function.clone(),
            });
        }
        if let (Some(bank), Some(found)) = (pp.bank, &a.iostandard) {
            if let Some(expected) = board.and_then(|b| b.bank_standard(bank)) {
                if found.starts_with("LVCMOS") && found != expected {
                    issues.push(Issue::Standard {
                        net: a.net.clone(),
                        pin: a.pin.clone(),
                        bank,
                        expected: expected.to_string(),
                        found: found.clone(),
                    });
                }
            }
        }
        if looks_like_clock(&key) && pp.is_user_io() && !pp.is_gclk() {
            issues.push(Issue::ClockNotGclk {
                net: a.net.clone(),
                pin: a.pin.clone(),
            });
        }
    }
    for (pin, nets) in by_pin {
        let distinct: HashSet<String> = nets.iter().map(|n| normalise(n)).collect();
        if distinct.len() > 1 {
            issues.push(Issue::UsedTwice {
                pin: pin.to_string(),
                nets: nets.iter().map(|s| s.to_string()).collect(),
            });
        }
    }
    issues.sort_by_key(|i| i.severity());
    issues
}

/// Can `net` go on `pin`? `Err` explains why not.
pub fn can_assign(
    net: &str,
    pin: &str,
    assigned: &[Assignment],
    package: &Package,
    board: Option<&Board>,
) -> Result<(), String> {
    let pp = package
        .pin(pin)
        .ok_or_else(|| format!("{pin} is not a ball of this package"))?;
    if let Some(reason) = board.and_then(|b| b.reserved_reason(pin)) {
        return Err(format!("{pin} is reserved: {reason}"));
    }
    if !pp.is_user_io() {
        return Err(format!("{pin} is {}, not a user I/O pin", pp.function));
    }
    let key = normalise(net);
    if let Some(other) = assigned
        .iter()
        .find(|a| a.pin.eq_ignore_ascii_case(pin) && normalise(&a.net) != key)
    {
        return Err(format!("{pin} already carries {}", other.net));
    }
    Ok(())
}

/// Put `net` on `pin`, editing its existing LOC line or appending one to
/// `new_file` (created if needed). `standard` sets the line's IOSTANDARD.
/// Returns the file that changed.
pub fn set_pin(
    assigned: &[Assignment],
    new_file: &Path,
    net: &str,
    pin: &str,
    standard: Option<&str>,
) -> std::io::Result<PathBuf> {
    let key = normalise(net);
    let pin = pin.to_ascii_uppercase();
    if let Some(a) = assigned.iter().find(|a| normalise(&a.net) == key) {
        let text = std::fs::read_to_string(&a.file)?;
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let i = a.line as usize - 1;
        let mut l = LOC_VALUE
            .replace(&lines[i], format!("${{1}}\"{pin}\""))
            .into_owned();
        if let Some(std) = standard {
            l = if IOSTD.is_match(&l) {
                IOSTD
                    .replace(&l, format!("IOSTANDARD = {std}"))
                    .into_owned()
            } else if let Some(semi) = l.rfind(';') {
                format!(
                    "{} | IOSTANDARD = {std} {}",
                    l[..semi].trim_end(),
                    &l[semi..]
                )
            } else {
                format!("{l} | IOSTANDARD = {std} ;")
            };
        }
        lines[i] = l;
        write_lines(&a.file, &lines, text.ends_with('\n'))?;
        return Ok(a.file.clone());
    }
    let mut text = std::fs::read_to_string(new_file).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.lines().any(|l| l.trim() == SECTION) {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(SECTION);
        text.push('\n');
    }
    let std = standard
        .map(|s| format!(" | IOSTANDARD = {s}"))
        .unwrap_or_default();
    text.push_str(&format!("NET \"{key}\" LOC = \"{pin}\"{std} ;\n"));
    if let Some(dir) = new_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(new_file, text)?;
    Ok(new_file.to_path_buf())
}

/// Remove `net`'s pin by commenting its LOC line out. Returns the file.
pub fn clear_pin(assigned: &[Assignment], net: &str) -> std::io::Result<Option<PathBuf>> {
    let key = normalise(net);
    let Some(a) = assigned.iter().find(|a| normalise(&a.net) == key) else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&a.file)?;
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let i = a.line as usize - 1;
    lines[i] = format!("# cleared by the pin planner: {}", lines[i]);
    write_lines(&a.file, &lines, text.ends_with('\n'))?;
    Ok(Some(a.file.clone()))
}

fn write_lines(path: &Path, lines: &[String], newline: bool) -> std::io::Result<()> {
    let mut s = lines.join("\n");
    if newline {
        s.push('\n');
    }
    std::fs::write(path, s)
}

/// Board labels for balls, from the board's master UCF (`D2` -> `LED<0>`).
pub fn board_labels(board: &Board) -> BTreeMap<String, String> {
    board
        .ucf_text()
        .map(|t| {
            ucf_pin_locs(t)
                .into_iter()
                .map(|l| (l.pin, l.net))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: &str = include_str!("../tests/fixtures/xc3s1500fg320-excerpt.pkg");

    fn rc10() -> &'static Board {
        fpgaflow_core::board::board("rc10").unwrap()
    }

    fn ports() -> Vec<PortBit> {
        port_bits([("clk", 1, "input"), ("led", 2, "output")])
    }

    #[test]
    fn port_bits_and_names() {
        let p = ports();
        let names: Vec<&str> = p.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["clk", "led<0>", "led<1>"]);
        assert_eq!(normalise("led[3]"), "led<3>");
    }

    #[test]
    fn checks() {
        let pkg = Package::parse(PKG);
        let ucf = "NET \"clk\" LOC = \"B5\" | IOSTANDARD = LVCMOS33 ;\n\
                   NET \"led<0>\" LOC = \"D2\" | IOSTANDARD = LVCMOS33 ;\n\
                   NET \"stale\" LOC = \"T15\" ;\n\
                   NET \"x\" LOC = \"D2\" ;\n";
        let a = read_assignments(&[(PathBuf::from("c.ucf"), ucf.to_string())]);
        assert_eq!(a[0].iostandard.as_deref(), Some("LVCMOS33"));
        let issues = check(&ports(), &a, &pkg, Some(rc10()));
        let text: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        let has = |s: &str| text.iter().any(|t| t.contains(s));
        assert!(has("led<1> has no pin"), "{text:#?}");
        assert!(has("T15 is CCLK, not a user I/O"), "{text:#?}");
        assert!(has("D2 is assigned to led<0> and x"), "{text:#?}");
        // D2 is in bank 7, which the RC10 runs at 2.5 V.
        assert!(
            has("bank 7 (LVCMOS25) but the UCF says LVCMOS33"),
            "{text:#?}"
        );
        // B5 is an ordinary I/O, not a GCLK.
        assert!(has("clk looks like a clock"), "{text:#?}");
        assert!(
            has("stale has a pin but is not a top-level port"),
            "{text:#?}"
        );
        assert_eq!(issues[0].severity(), Severity::Error);
    }

    #[test]
    fn reserved_and_occupied_pins_are_refused() {
        let pkg = Package::parse(PKG);
        let a = read_assignments(&[(
            PathBuf::from("c.ucf"),
            "NET \"led<0>\" LOC = \"D2\";".into(),
        )]);
        assert!(can_assign("led<1>", "P10", &a, &pkg, Some(rc10())).is_ok());
        assert!(can_assign("led<1>", "D2", &a, &pkg, Some(rc10()))
            .unwrap_err()
            .contains("already carries led<0>"));
        assert!(
            can_assign("led<0>", "D2", &a, &pkg, Some(rc10())).is_ok(),
            "re-assigning its own pin"
        );
        assert!(can_assign("x", "T15", &a, &pkg, Some(rc10()))
            .unwrap_err()
            .contains("CCLK"));
        assert!(can_assign("x", "ZZ9", &a, &pkg, Some(rc10())).is_err());
    }

    #[test]
    fn edits_keep_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("c.ucf");
        let original = "# my pins\nNET \"led<0>\" LOC = \"D2\" | IOSTANDARD = LVCMOS33 ;  # LED0\nNET \"clk\" PERIOD = 20ns;\n";
        std::fs::write(&f, original).unwrap();
        let read = || read_assignments(&read_files(std::slice::from_ref(&f)));

        // Move an existing pin: same line, new LOC and standard, comment kept.
        set_pin(&read(), &f, "led[0]", "e1", Some("LVCMOS25")).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(
            t,
            "# my pins\nNET \"led<0>\" LOC = \"E1\" | IOSTANDARD = LVCMOS25 ;  # LED0\nNET \"clk\" PERIOD = 20ns;\n"
        );

        // A new one goes into the planner section, once.
        set_pin(&read(), &f, "led<1>", "D3", Some("LVCMOS25")).unwrap();
        set_pin(&read(), &f, "clk", "P10", None).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(t.matches(SECTION).count(), 1);
        assert!(t.ends_with("NET \"led<1>\" LOC = \"D3\" | IOSTANDARD = LVCMOS25 ;\nNET \"clk\" LOC = \"P10\" ;\n"), "{t}");
        assert_eq!(read().len(), 3);

        // Clearing comments the line out.
        clear_pin(&read(), "led<1>").unwrap();
        assert_eq!(read().len(), 2);
        assert!(std::fs::read_to_string(&f)
            .unwrap()
            .contains("# cleared by the pin planner: NET \"led<1>\""));

        // A line without IOSTANDARD gets one before the ';'.
        set_pin(&read(), &f, "clk", "P10", Some("LVCMOS33")).unwrap();
        assert!(std::fs::read_to_string(&f)
            .unwrap()
            .contains("NET \"clk\" LOC = \"P10\" | IOSTANDARD = LVCMOS33 ;"));
    }

    #[test]
    fn rc10_board_labels() {
        let l = board_labels(rc10());
        assert_eq!(l.get("D2").map(String::as_str), Some("LED<0>"));
        assert_eq!(l.get("P10").map(String::as_str), Some("CLK_48MHZ"));
    }
}
