mod checks;

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use checks::{default_checks, run_check};

const INPUT: &str = "/opt/forge/mv/input/input.json";
const OUTPUT: &str = "/opt/forge/mv/output/result.json";

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
    if let Err(error) = run(INPUT, OUTPUT) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run(input_path: &str, output_path: &str) -> Result<(), String> {
    let input: Input =
        serde_json::from_slice(&fs::read(input_path).map_err(|e| format!("read input: {e}"))?)
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
        if let Some(finding) = run_check(&check)? {
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

fn write_result(path: &str, result: &ResultFile<'_>) -> Result<(), String> {
    let path = Path::new(path);
    let directory = path.parent().ok_or("result path has no parent")?;
    fs::create_dir_all(directory).map_err(|e| format!("create output directory: {e}"))?;
    let temporary = directory.join(".result.json.tmp");
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
        write_result(output.to_str().unwrap(), &result).unwrap();
        assert!(output.is_file());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
