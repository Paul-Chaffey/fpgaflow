//! Small things the GUI remembers between runs (not settings): the last
//! project folder. Kept in `$XDG_STATE_HOME/fpgaflow/state.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The folder of the last project opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_project: Option<PathBuf>,
}

fn path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("fpgaflow")
        .join("state.toml")
}

impl State {
    pub fn load() -> State {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Save (best effort).
    pub fn save(&self) {
        let p = path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(t) = toml::to_string(self) {
            let _ = std::fs::write(p, t);
        }
    }

    /// Where the Open Project dialog should start: the folder holding the
    /// last project (so its neighbours are one click away), else the
    /// examples folder.
    pub fn open_dir(&self) -> PathBuf {
        self.last_project
            .as_ref()
            .and_then(|p| p.parent())
            .filter(|p| p.is_dir())
            .map(PathBuf::from)
            .unwrap_or_else(crate::paths::examples_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_dir_prefers_the_last_project_folder() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        let s = State {
            last_project: Some(proj),
        };
        assert_eq!(s.open_dir(), dir.path());
        assert_eq!(State::default().open_dir(), crate::paths::examples_dir());
        let gone = State {
            last_project: Some("/no/such/dir/proj".into()),
        };
        assert_eq!(gone.open_dir(), crate::paths::examples_dir());
    }
}
