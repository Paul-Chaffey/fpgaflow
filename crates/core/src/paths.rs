//! Default folders.
//!
//! When fpgaflow is built from its source tree (as `scripts/install-desktop.sh`
//! does), that tree's `examples/` and `images/` are the natural starting
//! points; otherwise everything falls back to the home folder.

use std::path::{Path, PathBuf};

use crate::config::UserConfig;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// The fpgaflow source tree this binary was built from, if it still exists.
pub fn source_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .ok()?;
    root.join("examples").is_dir().then_some(root)
}

/// The example projects (the first place to look on a new install).
pub fn examples_dir() -> PathBuf {
    source_root()
        .map(|r| r.join("examples"))
        .unwrap_or_else(home)
}

/// Where flash images go: `[images_dir]` in config.toml, else the source
/// tree's `images/`, else `~/fpgaflow-images`.
pub fn images_dir(cfg: &UserConfig) -> PathBuf {
    cfg.images_dir
        .clone()
        .or_else(|| source_root().map(|r| r.join("images")))
        .unwrap_or_else(|| home().join("fpgaflow-images"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_from_the_source_tree() {
        // Tests run from the checkout, so both resolve inside it.
        let root = source_root().expect("source tree");
        assert_eq!(examples_dir(), root.join("examples"));
        let cfg = UserConfig {
            images_dir: None,
            ..UserConfig::default()
        };
        assert_eq!(images_dir(&cfg), root.join("images"));
        let own = UserConfig {
            images_dir: Some("/tmp/x".into()),
            ..UserConfig::default()
        };
        assert_eq!(images_dir(&own), PathBuf::from("/tmp/x"));
    }
}
