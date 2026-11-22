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
    pub ai: AiConfig,
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
            ai: AiConfig::default(),
            tool_paths: BTreeMap::new(),
            search_paths: vec![
                // openFPGALoader built with the XC3S700AN part entry; first so
                // it wins over the OSS CAD Suite copy.
                home.join("tools/openfpgaloader-fpgaflow/bin"),
                home.join("tools/oss-cad-suite/bin"),
                home.join("Xilinx/14.7/ISE_DS/ISE/bin/lin64"),
                PathBuf::from("/opt/Xilinx/14.7/ISE_DS/ISE/bin/lin64"),
            ],
        }
    }
}

/// Which route to Claude to use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AiBackend {
    /// The Claude Code CLI (`claude -p`), using its existing login.
    #[default]
    ClaudeCode,
    /// The Anthropic Messages API over HTTPS, with an API key.
    Api,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    pub backend: AiBackend,
    pub model: String,
    /// `low`, `medium`, `high`, `xhigh` or `max`.
    pub effort: String,
    /// Command that prints the API key (e.g. a KWallet or `pass` lookup).
    /// Used when `ANTHROPIC_API_KEY` is not set; the key is never stored here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_command: Option<String>,
    /// How many times generated code is sent back with its compile/sim errors.
    pub max_fix_rounds: u32,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            backend: AiBackend::ClaudeCode,
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            api_key_command: None,
            max_fix_rounds: 3,
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
