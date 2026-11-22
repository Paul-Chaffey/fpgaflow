//! FPGA family and P&R backend database (`data/families.toml`, compiled in).

use std::sync::LazyLock;

use serde::Deserialize;

const DATA: &str = include_str!("../../../data/families.toml");

/// Netlist format a backend consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetlistFormat {
    Json,
    Edif,
    Blif,
    Eblif,
    Vqm,
}

impl NetlistFormat {
    pub fn extension(self) -> &'static str {
        match self {
            NetlistFormat::Json => "json",
            NetlistFormat::Edif => "edf",
            NetlistFormat::Blif => "blif",
            NetlistFormat::Eblif => "eblif",
            NetlistFormat::Vqm => "vqm",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backend {
    pub id: String,
    pub name: String,
    pub netlist: NetlistFormat,
    pub open_source: bool,
}

/// How a family's synth command handles design hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Flatten {
    /// Flattens only when given `-flatten`.
    Flag,
    /// Flattens unless given `-noflatten`.
    Default,
    /// Accepts neither option.
    Fixed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// Yosys synthesis command(s), `;`-separated.
    pub yosys: String,
    pub top_flag: bool,
    pub flatten: Flatten,
    pub backends: Vec<String>,
    pub constraints: String,
    /// Set when the Yosys support is experimental; the text says why.
    #[serde(default)]
    pub experimental: Option<String>,
}

impl Family {
    pub fn default_backend(&self) -> &str {
        &self.backends[0]
    }
}

#[derive(Debug, Deserialize)]
struct Db {
    backend: Vec<Backend>,
    family: Vec<Family>,
}

static DB: LazyLock<Db> =
    LazyLock::new(|| toml::from_str(DATA).expect("data/families.toml is invalid"));

pub fn families() -> &'static [Family] {
    &DB.family
}

pub fn backends() -> &'static [Backend] {
    &DB.backend
}

pub fn family(id: &str) -> Option<&'static Family> {
    DB.family.iter().find(|f| f.id == id)
}

pub fn backend(id: &str) -> Option<&'static Backend> {
    DB.backend.iter().find(|b| b.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_is_consistent() {
        assert!(!families().is_empty());
        let mut ids: Vec<_> = families().iter().map(|f| f.id.as_str()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(n, ids.len(), "duplicate family id");
        for f in families() {
            assert!(!f.backends.is_empty(), "{} has no backends", f.id);
            for b in &f.backends {
                assert!(backend(b).is_some(), "{} names unknown backend {b}", f.id);
            }
        }
    }

    #[test]
    fn spartan3a_goes_through_ise_with_edif() {
        let f = family("spartan3a").unwrap();
        assert_eq!(f.default_backend(), "ise");
        assert_eq!(backend("ise").unwrap().netlist, NetlistFormat::Edif);
        assert!(f.yosys.contains("-family xc3sa"));
        assert!(f.experimental.is_some());
    }
}
