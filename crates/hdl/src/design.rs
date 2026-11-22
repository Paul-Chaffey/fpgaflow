//! Design hierarchy and port lists, from Yosys `write_json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::{self, Diagnostic};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Input,
    Output,
    Inout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Port {
    pub name: String,
    pub direction: Direction,
    pub width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Module {
    pub name: String,
    pub ports: Vec<Port>,
    pub is_top: bool,
    /// Yosys `src` attribute, e.g. `rtl/blinky.sv:2.1-17.10`.
    pub src: Option<String>,
    /// Names of modules this one instantiates.
    pub children: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Design {
    pub modules: Vec<Module>,
}

#[derive(Debug, thiserror::Error)]
pub enum DesignError {
    #[error("invalid Yosys JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot run yosys: {0}")]
    Io(#[from] std::io::Error),
    #[error("yosys failed ({} diagnostic(s))", diagnostics.len())]
    Yosys {
        diagnostics: Vec<Diagnostic>,
        log: String,
    },
}

// Only the parts of Yosys' JSON we use.
#[derive(Deserialize)]
struct YJson {
    modules: serde_json::Map<String, serde_json::Value>,
}
#[derive(Deserialize)]
struct YModule {
    #[serde(default)]
    attributes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    ports: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    cells: serde_json::Map<String, serde_json::Value>,
}
#[derive(Deserialize)]
struct YPort {
    direction: Direction,
    bits: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct YCell {
    #[serde(rename = "type")]
    kind: String,
}

/// Yosys writes boolean attributes as binary strings ("000...01") or ints.
fn truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.chars().any(|c| c == '1'),
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) != 0,
        _ => false,
    }
}

/// Strip Yosys' parameterised-module mangling: `$paramod\blinky\WIDTH=...` → `blinky`.
fn base_name(name: &str) -> &str {
    name.strip_prefix("$paramod")
        .and_then(|rest| rest.trim_start_matches('\\').split('\\').next())
        .unwrap_or(name)
}

impl Design {
    pub fn from_yosys_json(text: &str) -> Result<Self, DesignError> {
        let y: YJson = serde_json::from_str(text)?;
        let mut modules = Vec::new();
        for (name, value) in y.modules {
            if name.starts_with('$') && !name.starts_with("$paramod") {
                continue; // internal cell library modules
            }
            let m: YModule = serde_json::from_value(value)?;
            if m.attributes.get("blackbox").is_some_and(truthy) {
                continue;
            }
            let mut ports = Vec::new();
            for (pname, pv) in m.ports {
                let p: YPort = serde_json::from_value(pv)?;
                ports.push(Port {
                    name: pname,
                    direction: p.direction,
                    width: p.bits.len(),
                });
            }
            let mut children: Vec<String> = Vec::new();
            for (_, cv) in m.cells {
                let c: YCell = serde_json::from_value(cv)?;
                if c.kind.starts_with('$') && !c.kind.starts_with("$paramod") {
                    continue; // primitive cells
                }
                let child = base_name(c.kind.trim_start_matches('\\')).to_string();
                if !children.contains(&child) {
                    children.push(child);
                }
            }
            modules.push(Module {
                name: base_name(&name).to_string(),
                ports,
                is_top: m.attributes.get("top").is_some_and(truthy),
                src: m
                    .attributes
                    .get("src")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                children,
            });
        }
        Ok(Self { modules })
    }

    pub fn top(&self) -> Option<&Module> {
        self.modules.iter().find(|m| m.is_top)
    }

    pub fn module(&self, name: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.name == name)
    }
}

/// Inputs for [`analyse`].
#[derive(Debug, Clone, Default)]
pub struct AnalyseRequest {
    pub files: Vec<PathBuf>,
    pub include_dirs: Vec<PathBuf>,
    pub defines: BTreeMap<String, String>,
    /// `None`: let Yosys choose (`hierarchy -auto-top`).
    pub top: Option<String>,
    /// Use the yosys-slang frontend (`read_slang`) for full SystemVerilog.
    pub slang: bool,
}

fn quote(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

/// The Yosys script used for analysis; exposed for tests and the log.
pub fn analyse_script(req: &AnalyseRequest, json_out: &Path) -> String {
    let mut read = if req.slang {
        String::from("read_slang")
    } else {
        String::from("read_verilog -sv")
    };
    for d in &req.include_dirs {
        read.push_str(&format!(" -I {}", quote(d)));
    }
    for (k, v) in &req.defines {
        read.push_str(&format!(" -D{k}={v}"));
    }
    for f in &req.files {
        read.push(' ');
        read.push_str(&quote(f));
    }
    let hier = match &req.top {
        Some(t) => format!("hierarchy -check -top {t}"),
        None => "hierarchy -check -auto-top".to_string(),
    };
    // `proc` converts processes to netlist form; write_json refuses them otherwise.
    format!("{read}; {hier}; proc; write_json {}", quote(json_out))
}

/// Run Yosys to elaborate the design and return its hierarchy and ports.
/// `workdir` receives `design.json` and `analyse.log`.
pub fn analyse(yosys: &Path, req: &AnalyseRequest, workdir: &Path) -> Result<Design, DesignError> {
    std::fs::create_dir_all(workdir)?;
    let json = workdir.join("design.json");
    let script = analyse_script(req, &json);
    let out = std::process::Command::new(yosys)
        .arg("-q")
        .arg("-p")
        .arg(&script)
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .output()?;
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(workdir.join("analyse.log"), &log)?;
    if !out.status.success() {
        let diagnostics = log
            .lines()
            .filter_map(diag::parse_line)
            .map(|d| d.resolve(workdir))
            .collect();
        return Err(DesignError::Yosys { diagnostics, log });
    }
    Design::from_yosys_json(&std::fs::read_to_string(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{
      "creator": "Yosys",
      "modules": {
        "top": {
          "attributes": { "top": "00000000000000000000000000000001", "src": "rtl/top.sv:1.1-9.10" },
          "ports": {
            "clk": { "direction": "input", "bits": [ 2 ] },
            "rst": { "direction": "input", "bits": [ 3 ] },
            "leds": { "direction": "output", "bits": [ 4, 5, 6, 7 ] }
          },
          "cells": {
            "u0": { "type": "$paramod\\blinky\\WIDTH=s32'00000000000000000000000000011001" },
            "u1": { "type": "$paramod\\blinky\\WIDTH=s32'00000000000000000000000000000100" },
            "$and$x": { "type": "$and" }
          }
        },
        "$paramod\\blinky\\WIDTH=s32'00000000000000000000000000011001": {
          "attributes": { "src": "rtl/blinky.sv:2.1-17.10" },
          "ports": { "clk": { "direction": "input", "bits": [ 2 ] } }
        },
        "SB_LUT4": { "attributes": { "blackbox": "00000000000000000000000000000001" }, "ports": {} }
      }
    }"#;

    #[test]
    fn parses_modules_ports_and_top() {
        let d = Design::from_yosys_json(JSON).unwrap();
        let top = d.top().unwrap();
        assert_eq!(top.name, "top");
        let ports: Vec<_> = top
            .ports
            .iter()
            .map(|p| (p.name.as_str(), p.direction, p.width))
            .collect();
        assert_eq!(
            ports,
            [
                ("clk", Direction::Input, 1),
                ("rst", Direction::Input, 1),
                ("leds", Direction::Output, 4)
            ]
        );
        assert_eq!(top.children, ["blinky"]);
        assert!(d.module("blinky").is_some());
        assert!(d.module("SB_LUT4").is_none(), "blackboxes are skipped");
    }

    #[test]
    fn script_quotes_paths_and_sets_top() {
        let req = AnalyseRequest {
            files: vec!["/p/rtl/a b.sv".into()],
            include_dirs: vec!["/p/inc".into()],
            defines: [("W".to_string(), "8".to_string())].into(),
            top: Some("top".into()),
            slang: false,
        };
        let s = analyse_script(&req, Path::new("/p/build/d.json"));
        assert_eq!(
            s,
            r#"read_verilog -sv -I "/p/inc" -DW=8 "/p/rtl/a b.sv"; hierarchy -check -top top; proc; write_json "/p/build/d.json""#
        );
    }
}
