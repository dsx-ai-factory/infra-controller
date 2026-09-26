use std::{path::Path, process::Command};

use serde::Deserialize;
use serde_json::Value;

use crate::{Finding, RequestedCheck};

const MAX_DIAGNOSTIC_OUTPUT: usize = 4096;
type CheckHandler = fn(&Value) -> Result<Option<Finding>, String>;

const CHECKS: &[(&str, CheckHandler)] = &[("dcgm-diagnostic", run_dcgm_diagnostic)];

pub(crate) fn default_checks() -> Vec<RequestedCheck> {
    vec![RequestedCheck {
        name: "dcgm-diagnostic".to_owned(),
        parameters: serde_json::json!({ "runLevel": 3 }),
    }]
}

pub(crate) fn run_check(check: &RequestedCheck) -> Result<Option<Finding>, String> {
    let (_, handler) = CHECKS
        .iter()
        .find(|(name, _)| *name == check.name)
        .ok_or_else(|| format!("unsupported basic check {:?}", check.name))?;
    handler(&check.parameters)
}

fn run_dcgm_diagnostic(parameters: &Value) -> Result<Option<Finding>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Parameters {
        #[serde(default = "default_level")]
        run_level: u8,
    }
    let level = serde_json::from_value::<Parameters>(parameters.clone())
        .map_err(|error| format!("parse dcgm-diagnostic parameters: {error}"))?
        .run_level;
    if !matches!(level, 1 | 3) {
        return Err("dcgm-diagnostic runLevel must be 1 or 3".to_owned());
    }
    validate_host_dcgm(Path::new("/host"))?;
    let command = dcgm_command(Path::new("/host"), level)
        .output()
        .map_err(|error| format!("run host dcgmi: {error}"))?;
    let output = bounded_output(&command.stdout, &command.stderr);
    Ok((!command.status.success()).then(|| Finding {
        name: "dcgm-diagnostic",
        message: if output.is_empty() {
            format!("DCGM diagnostic level {level} failed")
        } else {
            output
        },
    }))
}

fn validate_host_dcgm(host_root: &Path) -> Result<(), String> {
    let path = host_root.join("usr/bin/dcgmi");
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("host DCGM is unavailable at {}", path.display()))
    }
}

fn dcgm_command(host_root: &Path, level: u8) -> Command {
    let mut command = Command::new("chroot");
    command
        .arg(host_root)
        .args(["/usr/bin/dcgmi", "diag", "-r"])
        .arg(level.to_string());
    command
}

fn default_level() -> u8 {
    3
}

fn bounded_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut output = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    output.truncate(MAX_DIAGNOSTIC_OUTPUT);
    output.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_dcgm_check() {
        let checks = default_checks();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "dcgm-diagnostic");
        assert_eq!(checks[0].parameters["runLevel"], 3);
    }

    #[test]
    fn rejects_unknown_checks() {
        let check = RequestedCheck {
            name: "unknown".to_owned(),
            parameters: serde_json::json!({}),
        };
        assert!(run_check(&check).unwrap_err().contains("unsupported"));
    }

    #[test]
    fn rejects_invalid_dcgm_level_before_execution() {
        let check = RequestedCheck {
            name: "dcgm-diagnostic".to_owned(),
            parameters: serde_json::json!({ "runLevel": 2 }),
        };
        assert!(run_check(&check).unwrap_err().contains("runLevel"));
    }

    #[test]
    fn bounds_diagnostic_output() {
        let output = bounded_output(&vec![b'x'; MAX_DIAGNOSTIC_OUTPUT + 1], b"");
        assert!(output.len() <= MAX_DIAGNOSTIC_OUTPUT);
    }

    #[test]
    fn rejects_missing_host_dcgm() {
        let root = std::env::temp_dir().join(format!("nico-basic-plugin-{}", std::process::id()));
        assert!(validate_host_dcgm(&root).unwrap_err().contains("host DCGM"));
    }

    #[test]
    fn constructs_host_dcgm_command() {
        let command = dcgm_command(Path::new("/host"), 3);
        assert_eq!(command.get_program(), "chroot");
        assert_eq!(command.get_args().collect::<Vec<_>>(), vec!["/host", "/usr/bin/dcgmi", "diag", "-r", "3"]);
    }
}
