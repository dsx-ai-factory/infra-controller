use std::{path::Path, process::Command};

use serde::Deserialize;
use serde_json::Value;

use crate::{Finding, RequestedCheck};

const MAX_DIAGNOSTIC_OUTPUT: usize = 4096;

pub(crate) fn default_checks() -> Vec<RequestedCheck> {
    vec![RequestedCheck {
        name: "dcgm-diagnostic".to_owned(),
        parameters: serde_json::json!({ "runLevel": 3 }),
    }]
}

pub(crate) fn run_check(check: &RequestedCheck) -> Result<Option<Finding>, String> {
    match check.name.as_str() {
        "dcgm-diagnostic" => run_dcgm_diagnostic(&check.parameters),
        _ => Err(format!("unsupported basic check {:?}", check.name)),
    }
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
    if !Path::new("/host/usr/bin/dcgmi").is_file() {
        return Err("host DCGM is unavailable at /host/usr/bin/dcgmi".to_owned());
    }
    let command = Command::new("chroot")
        .args(["/host", "/usr/bin/dcgmi", "diag", "-r", &level.to_string()])
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

fn default_level() -> u8 {
    3
}

fn bounded_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut output = format!("{}\n{}", String::from_utf8_lossy(stdout), String::from_utf8_lossy(stderr));
    output.truncate(MAX_DIAGNOSTIC_OUTPUT);
    output.trim().to_owned()
}
