mod checks;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{env, fs};

use checks::{default_checks, run_check};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const CONTRACT_DIR_ENV: &str = "NICO_MV_CONTRACT_DIR";
const DEFAULT_CONTRACT_DIR: &str = "/opt/forge/mv";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    contract_version: String,
    kind: String,
    #[serde(default)]
    parameters: Parameters,
}

#[derive(Default, Deserialize)]
struct Parameters {
    #[serde(default)]
    checks: Vec<RequestedCheck>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestedCheck {
    pub(crate) name: String,
    #[serde(default = "default_check_parameters")]
    pub(crate) parameters: Value,
}

fn default_check_parameters() -> Value {
    serde_json::json!({})
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResultFile<'a> {
    contract_version: &'static str,
    kind: &'static str,
    outcome: &'a str,
    severity: &'a str,
    summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<Finding>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Finding {
    pub(crate) name: &'static str,
    pub(crate) message: String,
}

fn main() {
    let (input_path, output_path) = contract_paths();
    if let Err(error) = run(&input_path, &output_path) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

/// Returns the contract paths selected by Scout, or the documented default for direct use.
fn contract_paths() -> (PathBuf, PathBuf) {
    contract_paths_from(env::var_os(CONTRACT_DIR_ENV))
}

/// Derives input and output paths from an optional container-visible contract directory.
fn contract_paths_from(contract_dir: Option<OsString>) -> (PathBuf, PathBuf) {
    let contract_dir = contract_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONTRACT_DIR));
    (
        contract_dir.join("input/input.json"),
        contract_dir.join("output/result.json"),
    )
}

/// Runs configured basic checks and publishes the Machine Validation result contract.
fn run(input_path: &Path, output_path: &Path) -> Result<(), String> {
    let input: Input = serde_json::from_slice(
        &fs::read(input_path)
            .map_err(|e| format!("read plugin input {}: {e}", input_path.display()))?,
    )
    .map_err(|e| format!("parse input: {e}"))?;
    if input.contract_version != "v1" || input.kind != "MachineValidationPluginInput" {
        return Err("unsupported plugin input contract".to_owned());
    }
    let checks = if input.parameters.checks.is_empty() {
        default_checks()
    } else {
        input.parameters.checks
    };
    let mut findings = Vec::new();
    for check in checks {
        let check_result = match run_check(&check) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("{error}");
                return write_result(
                    output_path,
                    &ResultFile {
                        contract_version: "v1",
                        kind: "MachineValidationPluginResult",
                        outcome: "error",
                        severity: "unknown",
                        summary: "basic check execution failed".to_owned(),
                        findings: Vec::new(),
                    },
                );
            }
        };
        if let Some(finding) = check_result {
            findings.push(finding);
        }
    }
    let result = if findings.is_empty() {
        ResultFile {
            contract_version: "v1",
            kind: "MachineValidationPluginResult",
            outcome: "pass",
            severity: "info",
            summary: "all basic checks passed".to_owned(),
            findings,
        }
    } else {
        ResultFile {
            contract_version: "v1",
            kind: "MachineValidationPluginResult",
            outcome: "fail",
            severity: "critical",
            summary: "one or more basic checks failed".to_owned(),
            findings,
        }
    };
    write_result(output_path, &result)
}

/// Atomically publishes the plugin result after the complete JSON document is written.
fn write_result(path: &Path, result: &ResultFile<'_>) -> Result<(), String> {
    let directory = path.parent().ok_or("result path has no parent")?;
    fs::create_dir_all(directory).map_err(|e| format!("create output directory: {e}"))?;
    let temporary = directory.join(format!(".result.json.{}.tmp", std::process::id()));
    fs::write(
        &temporary,
        serde_json::to_vec(result).map_err(|e| format!("serialize result: {e}"))?,
    )
    .map_err(|e| format!("write result: {e}"))?;
    fs::rename(temporary, path).map_err(|e| format!("publish result: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dcgm_check_levels() {
        for level in [1, 3] {
            let value: Input = serde_json::from_value(serde_json::json!({
                "contractVersion": "v1",
                "kind": "MachineValidationPluginInput",
                "parameters": { "checks": [{ "name": "dcgm-diagnostic", "parameters": { "runLevel": level } }] }
            }))
            .unwrap();
            assert_eq!(value.parameters.checks.len(), 1);
        }
    }

    #[test]
    fn rejects_unknown_parameters() {
        assert!(
            serde_json::from_str::<RequestedCheck>(
                r#"{"name":"dcgm-diagnostic","unexpected":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn writes_result_file() {
        let directory =
            std::env::temp_dir().join(format!("nico-basic-plugin-{}", std::process::id()));
        let output = directory.join("result.json");
        let result = ResultFile {
            contract_version: "v1",
            kind: "MachineValidationPluginResult",
            outcome: "pass",
            severity: "info",
            summary: "ok".to_owned(),
            findings: vec![],
        };
        write_result(&output, &result).unwrap();
        assert!(output.is_file());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn writes_an_error_result_for_a_handled_check_error() {
        let directory =
            std::env::temp_dir().join(format!("nico-basic-plugin-error-{}", std::process::id()));
        let input = directory.join("input.json");
        let output = directory.join("result.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            &input,
            r#"{
                "contractVersion": "v1",
                "kind": "MachineValidationPluginInput",
                "parameters": {
                    "checks": [{
                        "name": "dcgm-diagnostic",
                        "parameters": { "dcgmiPath": "/missing/dcgmi" }
                    }]
                }
            }"#,
        )
        .unwrap();

        run(&input, &output).expect("handled check error publishes a result");

        let result: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(result["outcome"], "error");
        assert_eq!(result["severity"], "unknown");
        assert_eq!(result["summary"], "basic check execution failed");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uses_the_configured_contract_directory() {
        let (input, output) = contract_paths_from(Some("/var/lib/nico/plugin-contract".into()));
        assert_eq!(
            input,
            Path::new("/var/lib/nico/plugin-contract/input/input.json")
        );
        assert_eq!(
            output,
            Path::new("/var/lib/nico/plugin-contract/output/result.json")
        );
    }
}
