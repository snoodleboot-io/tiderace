use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Defaults applied when neither a CLI flag nor a `[tool.riptide]` value is set.
pub const DEFAULT_PATTERN: &str = r"test_.*\.py|.*_test\.py";
pub const DEFAULT_DB: &str = ".riptide.db";
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Configuration parsed from `[tool.riptide]` in `pyproject.toml`. Every field is
/// optional; a CLI flag, when present, always takes precedence over these.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RiptideConfig {
    pub workers: Option<usize>,
    pub python: Option<String>,
    pub coverage: Option<bool>,
    pub pattern: Option<String>,
    pub db: Option<PathBuf>,
    pub paths: Option<Vec<PathBuf>>,
    pub timeout: Option<u64>,
}

#[derive(Deserialize)]
struct PyProject {
    tool: Option<Tool>,
}

#[derive(Deserialize)]
struct Tool {
    riptide: Option<RiptideConfig>,
}

impl RiptideConfig {
    /// Load `[tool.riptide]` from the given `pyproject.toml`. A missing file or a
    /// file with no `[tool.riptide]` section yields the default (all-`None`)
    /// config; a malformed file or unknown key is a hard error so typos surface.
    pub fn load(pyproject: &Path) -> Result<RiptideConfig> {
        if !pyproject.exists() {
            return Ok(RiptideConfig::default());
        }
        let text = std::fs::read_to_string(pyproject)
            .with_context(|| format!("reading {}", pyproject.display()))?;
        let parsed: PyProject = toml::from_str(&text)
            .with_context(|| format!("parsing [tool.riptide] in {}", pyproject.display()))?;
        Ok(parsed.tool.and_then(|t| t.riptide).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> RiptideConfig {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pyproject.toml");
        std::fs::write(&p, src).unwrap();
        RiptideConfig::load(&p).unwrap()
    }

    #[test]
    fn missing_file_is_default() {
        let cfg = RiptideConfig::load(Path::new("/no/such/pyproject.toml")).unwrap();
        assert_eq!(cfg, RiptideConfig::default());
    }

    #[test]
    fn no_section_is_default() {
        assert_eq!(
            parse("[tool.black]\nline-length = 88\n"),
            RiptideConfig::default()
        );
    }

    #[test]
    fn reads_all_fields() {
        let cfg = parse(
            r#"
[tool.riptide]
workers = 8
python = ".venv/bin/python"
coverage = true
pattern = "check_.*\\.py"
db = ".state.db"
paths = ["tests", "extra"]
timeout = 60
"#,
        );
        assert_eq!(cfg.workers, Some(8));
        assert_eq!(cfg.python.as_deref(), Some(".venv/bin/python"));
        assert_eq!(cfg.coverage, Some(true));
        assert_eq!(cfg.pattern.as_deref(), Some(r"check_.*\.py"));
        assert_eq!(cfg.db, Some(PathBuf::from(".state.db")));
        assert_eq!(
            cfg.paths,
            Some(vec![PathBuf::from("tests"), PathBuf::from("extra")])
        );
        assert_eq!(cfg.timeout, Some(60));
    }

    #[test]
    fn unknown_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pyproject.toml");
        std::fs::write(&p, "[tool.riptide]\nworkrs = 8\n").unwrap();
        assert!(RiptideConfig::load(&p).is_err());
    }
}
