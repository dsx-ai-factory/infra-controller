mod resolver;

use std::fmt;
use std::path::Path;
use std::str::FromStr;

pub use resolver::resolve_artifact_urls;
use serde::{Deserialize, Deserializer};

/// A single, safe path component for an artifact's cache filename.
///
/// `output` is joined into the on-disk cache path and then served over
/// HTTP, so a value like `../../etc/passwd` would let a scenario write
/// outside the cache directory. This type rejects anything that isn't a
/// plain filename: empty, `.`, `..`, NUL, and path separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSegment(String);

impl PathSegment {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PathSegment {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err("must not be empty".to_string());
        }
        if s == "." || s == ".." {
            return Err(format!("'{s}' is a directory traversal component"));
        }
        if let Some(c) = s.chars().find(|&c| c == '/' || c == '\\' || c == '\0') {
            return Err(format!("contains disallowed character {c:?}"));
        }
        Ok(PathSegment(s.to_string()))
    }
}

impl<'de> Deserialize<'de> for PathSegment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Rack model + SOT release this scenario targets.
#[derive(Debug, Deserialize)]
pub struct RackTarget {
    pub model: String,
    pub sot_release: String,
}

/// Ephemeral OS image to boot on validation nodes.
#[derive(Debug, Deserialize)]
pub struct OsImage {
    pub uri: String,
}

/// Pre-cached artifact -- resolved via direct URI or SOT JSONPath.
#[derive(Debug, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub output: PathSegment,
    /// Direct download URL (mutually exclusive with `sotpath`).
    ///
    /// Exactly one of `uri`/`sotpath` must be set; enforced in
    /// `Scenario::load` after deserialization.
    pub uri: Option<String>,
    /// JSONPath into SOT JSON to resolve download URL.
    pub sotpath: Option<String>,
}

/// Setup step -- runs before tests, aborts validation on failure.
#[derive(Debug, Deserialize)]
pub struct SetupStep {
    pub execute: String,
}

/// Test step -- result recorded independently under `name`.
#[derive(Debug, Deserialize)]
pub struct TestStep {
    pub name: String,
    pub execute: String,
}

/// Teardown step -- always runs, regardless of test outcome.
#[derive(Debug, Deserialize)]
pub struct TeardownStep {
    pub execute: String,
}

/// Complete rack validation scenario definition.
#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub rack: RackTarget,
    pub os: OsImage,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub setup: Vec<SetupStep>,
    #[serde(default)]
    pub test: Vec<TestStep>,
    #[serde(default)]
    pub teardown: Vec<TeardownStep>,
}

impl Scenario {
    /// Parse a scenario from a TOML file on disk.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let scenario: Scenario =
            toml::from_str(&content).map_err(|e| format!("parse {}: {e}", path.display()))?;
        scenario
            .validate()
            .map_err(|e| format!("validate {}: {e}", path.display()))?;
        Ok(scenario)
    }

    fn validate(&self) -> Result<(), String> {
        for artifact in &self.artifacts {
            match (&artifact.uri, &artifact.sotpath) {
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "artifact '{}': both 'uri' and 'sotpath' set; exactly one required",
                        artifact.name
                    ));
                }
                (None, None) => {
                    return Err(format!(
                        "artifact '{}': neither 'uri' nor 'sotpath' set; exactly one required",
                        artifact.name
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_example_scenario() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("doc/example_scenario.toml");
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(scenario.rack.model, "gb200nvl");
        assert_eq!(scenario.rack.sot_release, "1.2.5");
        assert!(!scenario.os.uri.is_empty());
        assert_eq!(scenario.artifacts.len(), 6);
        assert_eq!(scenario.setup.len(), 1);
        assert_eq!(scenario.test.len(), 2);
        assert_eq!(scenario.teardown.len(), 1);
        assert_eq!(scenario.test[0].name, "nv_basic");
    }

    fn scenario_with_artifacts(artifacts: Vec<Artifact>) -> Scenario {
        Scenario {
            rack: RackTarget {
                model: "gb200nvl".to_string(),
                sot_release: "1.2.5".to_string(),
            },
            os: OsImage {
                uri: "https://example.com/os.img".to_string(),
            },
            artifacts,
            setup: vec![],
            test: vec![],
            teardown: vec![],
        }
    }

    fn artifact(name: &str, uri: Option<&str>, sotpath: Option<&str>) -> Artifact {
        Artifact {
            name: name.to_string(),
            output: format!("{name}.bin").parse().unwrap(),
            uri: uri.map(str::to_string),
            sotpath: sotpath.map(str::to_string),
        }
    }

    #[test]
    fn validate_accepts_uri_only() {
        let s = scenario_with_artifacts(vec![artifact("a", Some("https://x/y"), None)]);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_accepts_sotpath_only() {
        let s = scenario_with_artifacts(vec![artifact("a", None, Some("$.foo"))]);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_both_uri_and_sotpath() {
        let s = scenario_with_artifacts(vec![artifact("a", Some("https://x/y"), Some("$.foo"))]);
        let err = s.validate().unwrap_err();
        assert!(err.contains("both"), "got: {err}");
        assert!(err.contains("'a'"), "got: {err}");
    }

    #[test]
    fn validate_rejects_neither_uri_nor_sotpath() {
        let s = scenario_with_artifacts(vec![artifact("a", None, None)]);
        let err = s.validate().unwrap_err();
        assert!(err.contains("neither"), "got: {err}");
        assert!(err.contains("'a'"), "got: {err}");
    }

    // --- PathSegment ---

    #[test]
    fn path_segment_accepts_plain_filename() {
        assert_eq!(
            "rm_driver.run".parse::<PathSegment>().unwrap().as_str(),
            "rm_driver.run"
        );
        assert_eq!(
            ".hidden".parse::<PathSegment>().unwrap().as_str(),
            ".hidden"
        );
    }

    #[test]
    fn path_segment_rejects_traversal_and_separators() {
        for bad in ["", ".", "..", "../etc/passwd", "a/b", "a\\b", "a\0b"] {
            assert!(
                bad.parse::<PathSegment>().is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn path_segment_deserialize_rejects_traversal() {
        let toml = r#"
            [rack]
            model = "gb200nvl"
            sot_release = "1.2.5"
            [os]
            uri = "https://example.com/os.img"
            [[artifacts]]
            name = "evil"
            output = "../../etc/passwd"
            uri = "https://example.com/x"
        "#;
        let err = toml::from_str::<Scenario>(toml).unwrap_err();
        assert!(
            err.to_string().contains("disallowed character"),
            "got: {err}"
        );
    }
}
