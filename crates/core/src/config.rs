//! Per-user configuration: `~/.config/fpgaflow/config.toml`.
//!
//! This file is edited in the same external editor as the HDL sources.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
    pub editor: EditorConfig,
    /// Explicit path for a tool, keyed by tool id (see `tools::TOOLS`).
    pub tool_paths: BTreeMap<String, PathBuf>,
    /// Directories searched before `$PATH`, e.g. the OSS CAD Suite and ISE.
    pub search_paths: Vec<PathBuf>,
}

impl Default for UserConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            editor: EditorConfig::default(),
            tool_paths: BTreeMap::new(),
            search_paths: vec![
                home.join("tools/oss-cad-suite/bin"),
                PathBuf::from("/opt/Xilinx/14.7/ISE_DS/ISE/bin/lin64"),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum EditorPreset {
    #[default]
    Kate,
    Kwrite,
    Vscode,
    Gvim,
    Emacsclient,
    Custom,
}

impl EditorPreset {
    pub const ALL: [EditorPreset; 6] = [
        EditorPreset::Kate,
        EditorPreset::Kwrite,
        EditorPreset::Vscode,
        EditorPreset::Gvim,
        EditorPreset::Emacsclient,
        EditorPreset::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EditorPreset::Kate => "Kate",
            EditorPreset::Kwrite => "KWrite",
            EditorPreset::Vscode => "VS Code",
            EditorPreset::Gvim => "gVim",
            EditorPreset::Emacsclient => "Emacs (emacsclient)",
            EditorPreset::Custom => "Custom command",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    pub preset: EditorPreset,
    /// Used when `preset = "custom"`. Placeholders: `{file}`, `{line}`, `{col}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Used when `preset = "custom"` and no position is known. Placeholder: `{file}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_no_position: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot serialise config: {0}")]
    Serialise(#[from] toml::ser::Error),
}

impl UserConfig {
    /// `~/.config/fpgaflow/config.toml`.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("fpgaflow")
            .join("config.toml")
    }

    /// Load from `path`; a missing file gives the defaults.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&Self::default_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Write the defaults if no config file exists yet; returns its path.
    pub fn ensure_exists() -> Result<PathBuf, ConfigError> {
        let path = Self::default_path();
        if !path.exists() {
            Self::default().save_to(&path)?;
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = UserConfig::load_from(&dir.path().join("none.toml")).unwrap();
        assert_eq!(cfg.editor.preset, EditorPreset::Kate);
    }

    #[test]
    fn save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let mut cfg = UserConfig::default();
        cfg.editor.preset = EditorPreset::Custom;
        cfg.editor.command = Some("myedit +{line} {file}".into());
        cfg.tool_paths.insert("yosys".into(), "/x/yosys".into());
        cfg.save_to(&path).unwrap();
        assert_eq!(UserConfig::load_from(&path).unwrap(), cfg);
    }
}
