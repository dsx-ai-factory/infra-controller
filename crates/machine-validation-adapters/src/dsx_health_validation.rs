/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Native HealthValidation input preparation, host dependencies, and aggregate verdict.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    AdapterContext, AdapterInvocation, AdapterOutput, ContainerPluginAdapter, HostSocketMount,
    PluginResult, ReadOnlyMount, RuntimeRequirements,
};

pub(super) struct DsxHealthValidation;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Parameters {
    gpu_count: Option<u32>,
}

impl Parameters {
    fn parse(value: &Value) -> Result<Self, String> {
        if value.get("gpu_count").is_some_and(Value::is_null) {
            return Err("gpu_count must be an unsigned 32-bit integer when provided".to_owned());
        }
        serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid DSXHealthValidation parameters: {error}"))
    }

    fn operator_config(&self) -> Value {
        match self.gpu_count {
            Some(count) => serde_json::json!({"checks":{"gpu_count":{"expected_gpu_count":count}}}),
            None => serde_json::json!({}),
        }
    }
}

impl ContainerPluginAdapter for DsxHealthValidation {
    fn requirements(&self) -> RuntimeRequirements {
        RuntimeRequirements {
            all_gpus: true,
            run_as_root: true,
            host_network: true,
            host_pid: true,
            host_sockets: true,
        }
    }

    fn validate_registration(
        &self,
        entrypoint: &[String],
        parameters: &Value,
    ) -> Result<(), String> {
        Parameters::parse(parameters)?;
        let executable = entrypoint
            .first()
            .ok_or_else(|| "HealthValidation requires an entrypoint".to_owned())?;
        if !Path::new(executable).is_absolute() {
            return Err(
                "HealthValidation entrypoint must be an absolute container path".to_owned(),
            );
        }
        if entrypoint.iter().skip(1).any(|argument| {
            argument == "--"
                || (argument.starts_with('-')
                    && matches!(
                        argument.trim_start_matches('-').split('=').next(),
                        Some("node-id" | "config")
                    ))
        }) {
            return Err("HealthValidation node-id and config arguments are managed by NICo; an argument separator is not allowed".to_owned());
        }
        Ok(())
    }

    fn prepare<'a>(
        &'a self,
        context: AdapterContext<'a>,
        entrypoint: &'a [String],
    ) -> BoxFuture<'a, Result<AdapterInvocation, String>> {
        Box::pin(async move {
            self.validate_registration(entrypoint, &context.input["parameters"])?;
            let arguments = prepare_input(context, entrypoint)?;
            let journalctl = find_journalctl()?;
            let dcgm = discover_dcgm_library().await?;
            let mut mounts = journal_mounts(
                &journalctl,
                &inspect_dependencies(&journalctl).await?,
                &[Path::new("/run/log/journal"), Path::new("/var/log/journal")],
                Path::new("/etc/machine-id"),
            )?;
            for mount in library_mounts(&dcgm, &inspect_dependencies(&dcgm).await?)? {
                if !mounts
                    .iter()
                    .any(|existing| existing.target == mount.target)
                {
                    mounts.push(mount);
                }
            }
            let socket_mounts = audit_socket_mounts(
                &Parameters::parse(&context.input["parameters"])?,
                Path::new("/run/systemd/journal/socket"),
            )?;
            Ok(invocation(
                entrypoint,
                arguments,
                mounts,
                socket_mounts,
                &journalctl,
                &dcgm,
            ))
        })
    }

    fn interpret(
        &self,
        _context: AdapterContext<'_>,
        output: AdapterOutput<'_>,
    ) -> Result<PluginResult, String> {
        if output.stdout_truncated {
            return Err("HealthValidation verdict exceeds the stdout capture limit".to_owned());
        }
        if !matches!(output.exit_code, Some(0 | 1)) {
            return Err(format!(
                "HealthValidation did not complete an assessment (exit {:?})",
                output.exit_code
            ));
        }
        let report: Value = serde_json::from_slice(output.stdout)
            .map_err(|error| format!("invalid HealthValidation JSON verdict: {error}"))?;
        let overall = report
            .get("overall_severity")
            .and_then(Value::as_str)
            .ok_or_else(|| "HealthValidation requires a string overall_severity".to_owned())?;
        let (outcome, severity) = match overall {
            "PASS" => ("pass", None),
            "INFO" => ("pass", Some("info")),
            "WARNING" => ("pass", Some("warning")),
            "CRITICAL" => ("fail", Some("critical")),
            "UNKNOWN" => ("fail", Some("unknown")),
            _ => return Err("unrecognized HealthValidation overall_severity".to_owned()),
        };
        Ok(PluginResult {
            contract_version: "v1".to_owned(),
            kind: "MachineValidationPluginResult".to_owned(),
            outcome: outcome.to_owned(),
            summary: format!("HealthValidation {overall}"),
            severity: severity.map(str::to_owned),
            findings: Vec::new(),
        })
    }
}

fn prepare_input(
    context: AdapterContext<'_>,
    entrypoint: &[String],
) -> Result<Vec<String>, String> {
    let parameters = Parameters::parse(&context.input["parameters"])?;
    let machine_id = context.input["machineId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "HealthValidation requires a machine ID".to_owned())?;
    let config = serde_json::to_vec(&parameters.operator_config())
        .map_err(|error| format!("cannot serialize HealthValidation configuration: {error}"))?;
    // JSON is valid YAML and avoids another serializer for the operator file.
    std::fs::write(context.input_dir.join("operator.yaml"), config)
        .map_err(|error| format!("cannot write HealthValidation configuration: {error}"))?;
    let mut arguments: Vec<_> = entrypoint.iter().skip(1).cloned().collect();
    arguments.extend([
        "--node-id".to_owned(),
        machine_id.to_owned(),
        "--config".to_owned(),
        context
            .contract_dir
            .join("input/operator.yaml")
            .display()
            .to_string(),
    ]);
    // Execution context is a user argument: 0.2.0-rc1 does not support that flag.
    Ok(arguments)
}

fn invocation(
    entrypoint: &[String],
    arguments: Vec<String>,
    mounts: Vec<ReadOnlyMount>,
    socket_mounts: Vec<HostSocketMount>,
    journalctl: &Path,
    dcgm: &Path,
) -> AdapterInvocation {
    let journal_bin_dir = journalctl.parent().unwrap_or(Path::new("/usr/bin"));
    AdapterInvocation {
        entrypoint: entrypoint.first().cloned(),
        arguments,
        mounts,
        socket_mounts,
        environment: BTreeMap::from([
            (
                "LD_LIBRARY_PATH".to_owned(),
                dcgm.parent()
                    .expect("absolute DCGM library path has a parent")
                    .display()
                    .to_string(),
            ),
            (
                "NVIDIA_DRIVER_CAPABILITIES".to_owned(),
                "utility".to_owned(),
            ),
            (
                "PATH".to_owned(),
                format!(
                    "{}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    journal_bin_dir.display()
                ),
            ),
        ]),
    }
}

async fn inspect_dependencies(path: &Path) -> Result<String, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("ldd")
            .arg(path)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("timed out inspecting host dependency {}", path.display()))?
    .map_err(|error| format!("cannot inspect host dependency {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "ldd could not inspect host dependency {}",
            path.display()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn discover_dcgm_library() -> Result<PathBuf, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("ldconfig")
            .arg("-p")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "timed out discovering the host DCGM library".to_owned())?
    .map_err(|error| format!("cannot discover the host DCGM library with ldconfig: {error}"))?;
    if !output.status.success() {
        return Err("ldconfig could not inspect the host library cache".to_owned());
    }
    dcgm_library_from_cache(&String::from_utf8_lossy(&output.stdout))
}

fn dcgm_library_from_cache(cache: &str) -> Result<PathBuf, String> {
    for line in cache.lines() {
        let Some((metadata, path)) = line.split_once("=>") else {
            continue;
        };
        if metadata.split_whitespace().next() != Some("libdcgm.so.4") {
            continue;
        }
        let path = Path::new(path.trim());
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
    }
    Err(
        "HealthValidation requires host DCGM 4: libdcgm.so.4 must be present in the ldconfig cache"
            .to_owned(),
    )
}

fn library_mounts(library: &Path, ldd_output: &str) -> Result<Vec<ReadOnlyMount>, String> {
    let mut mounts = vec![file_mount(library, library)?];
    for dependency in dependency_paths_from_ldd(ldd_output)? {
        mounts.push(file_mount(&dependency, &dependency)?);
    }
    Ok(mounts)
}

fn audit_socket_mounts(
    parameters: &Parameters,
    path: &Path,
) -> Result<Vec<HostSocketMount>, String> {
    if parameters.gpu_count.is_none() {
        return Ok(Vec::new());
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "HealthValidation threshold overrides require the host journald socket {}: {error}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if metadata.file_type().is_socket() {
            return Ok(vec![HostSocketMount {
                source: std::fs::canonicalize(path).map_err(|error| error.to_string())?,
                target: path.to_owned(),
            }]);
        }
    }
    let _ = metadata;
    Err("HealthValidation threshold audit path must be a Unix socket".to_owned())
}

fn journal_mounts(
    journalctl: &Path,
    ldd_output: &str,
    journal_roots: &[&Path],
    machine_id: &Path,
) -> Result<Vec<ReadOnlyMount>, String> {
    let mut mounts = vec![file_mount(journalctl, journalctl)?];
    for library in dependency_paths_from_ldd(ldd_output)? {
        mounts.push(file_mount(&library, &library)?);
    }
    let journals: Vec<_> = journal_roots
        .iter()
        .copied()
        .filter(|path| path.is_dir())
        .collect();
    if journals.is_empty() {
        return Err("HealthValidation requires a host systemd journal directory".to_owned());
    }
    for path in journals {
        mounts.push(ReadOnlyMount {
            source: path.to_owned(),
            target: path.to_owned(),
        });
    }
    mounts.push(file_mount(machine_id, machine_id)?);
    Ok(mounts)
}

fn find_journalctl() -> Result<PathBuf, String> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join("journalctl"))
        .find(|path| {
            if !path.is_absolute() {
                return false;
            }
            let Ok(metadata) = path.metadata() else {
                return false;
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                metadata.is_file()
            }
        })
        .ok_or_else(|| "HealthValidation requires journalctl on the Scout host PATH".to_owned())
}

fn file_mount(source: &Path, target: &Path) -> Result<ReadOnlyMount, String> {
    let source = std::fs::canonicalize(source).map_err(|error| {
        format!(
            "HealthValidation dependency {} is unavailable: {error}",
            source.display()
        )
    })?;
    if !source.is_file() {
        return Err(format!(
            "HealthValidation dependency {} must be a regular file",
            source.display()
        ));
    }
    Ok(ReadOnlyMount {
        source,
        target: target.to_owned(),
    })
}

fn dependency_paths_from_ldd(output: &str) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.get(1) != Some(&"=>") {
            continue;
        }
        if fields.get(2..4) == Some(&["not", "found"][..]) {
            return Err(format!(
                "host dependency has an unresolved library: {}",
                fields[0]
            ));
        }
        // Keep the container's glibc and loader, matching the tested Scout invocation.
        if [
            "libc.so",
            "libpthread.so",
            "libdl.so",
            "librt.so",
            "libm.so",
        ]
        .iter()
        .any(|prefix| fields[0].starts_with(prefix))
        {
            continue;
        }
        let path = fields
            .get(2)
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| format!("invalid host library path in {line:?}"))?;
        if !paths.iter().any(|existing| existing == path) {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(input: &'a Value, directory: &'a Path) -> AdapterContext<'a> {
        AdapterContext {
            input,
            input_dir: directory,
            output_dir: directory,
            contract_dir: Path::new("/contract"),
        }
    }

    #[test]
    fn configuration_rejects_invalid_parameters_and_managed_argument_conflicts() {
        let command = vec!["/custom/hv".to_owned(), "run".to_owned()];
        for parameters in [
            serde_json::json!({"gpu_count":null}),
            serde_json::json!({"gpu_count":-1}),
            serde_json::json!({"gpu_count":"4"}),
            serde_json::json!({"gpu_count":4294967296_u64}),
            serde_json::json!({"gpuCount":4}),
        ] {
            assert!(
                DsxHealthValidation
                    .validate_registration(&command, &parameters)
                    .is_err()
            );
        }
        for argument in ["--node-id=other", "-config", "--config", "--"] {
            let mut command = command.clone();
            command.push(argument.to_owned());
            assert!(
                DsxHealthValidation
                    .validate_registration(&command, &serde_json::json!({}))
                    .is_err(),
                "{argument}"
            );
        }
        DsxHealthValidation
            .validate_registration(&command, &serde_json::json!({"gpu_count":4}))
            .unwrap();
    }

    #[test]
    fn configurable_command_and_operator_input_work_without_version_specific_flags() {
        let directory = std::env::temp_dir().join(format!("dsx-input-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let input = serde_json::json!({"machineId":"machine-1", "parameters":{"gpu_count":4}});
        let command = vec!["/custom/hv".to_owned(), "run".to_owned()];
        let arguments = prepare_input(context(&input, &directory), &command).unwrap();
        assert_eq!(
            arguments,
            [
                "run",
                "--node-id",
                "machine-1",
                "--config",
                "/contract/input/operator.yaml"
            ]
        );
        let config: Value =
            serde_json::from_slice(&std::fs::read(directory.join("operator.yaml")).unwrap())
                .unwrap();
        assert_eq!(config["checks"]["gpu_count"]["expected_gpu_count"], 4);
        let invocation = invocation(
            &command,
            arguments,
            vec![],
            vec![],
            Path::new("/usr/bin/journalctl"),
            Path::new("/usr/lib/libdcgm.so.4"),
        );
        assert_eq!(invocation.entrypoint.as_deref(), Some("/custom/hv"));
        assert_eq!(
            invocation.environment["NVIDIA_DRIVER_CAPABILITIES"],
            "utility"
        );
        assert!(DsxHealthValidation.requirements().all_gpus);
        assert!(DsxHealthValidation.requirements().run_as_root);
        assert!(DsxHealthValidation.requirements().host_network);
        assert!(DsxHealthValidation.requirements().host_pid);
        assert!(DsxHealthValidation.requirements().host_sockets);
        assert_eq!(invocation.environment["LD_LIBRARY_PATH"], "/usr/lib");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn journal_dependencies_include_binary_non_glibc_libraries_data_and_identity() {
        let directory = std::env::temp_dir().join(format!("dsx-journal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let binary = directory.join("journalctl");
        let library = directory.join("libsystemd.so.0");
        let identity = directory.join("machine-id");
        let journal = directory.join("journal");
        for file in [&binary, &library, &identity] {
            std::fs::write(file, b"fixture").unwrap();
        }
        std::fs::create_dir(&journal).unwrap();
        let ldd = format!(
            "libsystemd.so.0 => {} (0x01)\nlibc.so.6 => /container/glibc (0x02)\n/lib64/ld-linux.so (0x03)",
            library.display()
        );
        let mounts = journal_mounts(
            &binary,
            &ldd,
            &[&directory.join("missing"), &journal],
            &identity,
        )
        .unwrap();
        assert_eq!(
            mounts.iter().map(|mount| &mount.target).collect::<Vec<_>>(),
            [&binary, &library, &journal, &identity]
        );
        assert!(journal_mounts(&binary, &ldd, &[], &identity).is_err());
        std::fs::remove_file(&identity).unwrap();
        assert!(journal_mounts(&binary, &ldd, &[&journal], &identity).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unresolved_journal_libraries_are_preparation_errors() {
        assert!(dependency_paths_from_ldd("libsystemd.so => not found").is_err());
        assert!(dependency_paths_from_ldd("libsystemd.so => relative/path (0x01)").is_err());
    }

    #[test]
    fn dcgm_discovery_and_dependencies_require_version_four_without_replacing_glibc() {
        let directory = std::env::temp_dir().join(format!("dsx-dcgm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let library = directory.join("libdcgm.so.4");
        let dependency = directory.join("libnuma.so.1");
        for path in [&library, &dependency] {
            std::fs::write(path, b"fixture").unwrap();
        }
        let cache = format!(
            "libdcgm.so.3 (libc6,x86-64) => /old/libdcgm.so.3\nlibdcgm.so.4 (libc6,x86-64) => {}",
            library.display()
        );
        assert_eq!(dcgm_library_from_cache(&cache).unwrap(), library);
        for cache in [
            "",
            "libdcgm.so.3 (libc6) => /old/libdcgm.so.3",
            "libdcgm.so.4 (libc6) => relative/libdcgm.so.4",
        ] {
            assert!(dcgm_library_from_cache(cache).is_err());
        }
        let ldd = format!(
            "libnuma.so.1 => {} (0x01)\nlibc.so.6 => /container/glibc (0x02)",
            dependency.display()
        );
        let mounts = library_mounts(&library, &ldd).unwrap();
        assert_eq!(
            mounts.iter().map(|mount| &mount.target).collect::<Vec<_>>(),
            [&library, &dependency]
        );
        assert!(library_mounts(&library, "libnuma.so.1 => not found").is_err());
        std::fs::remove_file(&library).unwrap();
        assert!(library_mounts(&library, &ldd).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn threshold_overrides_require_a_real_journald_socket_but_defaults_do_not() {
        use std::os::unix::net::UnixDatagram;
        let directory = std::env::temp_dir().join(format!("dsx-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("socket");
        let overrides = Parameters { gpu_count: Some(4) };
        assert!(
            audit_socket_mounts(&Parameters::default(), &path)
                .unwrap()
                .is_empty()
        );
        assert!(audit_socket_mounts(&overrides, &path).is_err());
        std::fs::write(&path, b"not a socket").unwrap();
        assert!(audit_socket_mounts(&overrides, &path).is_err());
        std::fs::remove_file(&path).unwrap();
        let _socket = UnixDatagram::bind(&path).unwrap();
        assert_eq!(
            audit_socket_mounts(&overrides, &path).unwrap(),
            vec![HostSocketMount {
                source: std::fs::canonicalize(&path).unwrap(),
                target: path
            }]
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn overall_severity_is_the_only_health_signal() {
        let input = serde_json::json!({"machineId":"machine-1", "parameters":{"gpu_count":8}});
        for (overall, exit, expected, severity) in [
            ("PASS", 1, "pass", None),
            ("INFO", 0, "pass", Some("info")),
            ("WARNING", 0, "pass", Some("warning")),
            ("CRITICAL", 0, "fail", Some("critical")),
            ("UNKNOWN", 1, "fail", Some("unknown")),
        ] {
            // Metadata and per-check enums may differ across image versions.
            let stdout = serde_json::to_vec(&serde_json::json!({"schema_version":"0.2", "node_id":"other-node", "overall_severity":overall,
                "check_results":[{"severity":"CRITICAL", "recommended_action":{"operational_state":"none"}}],
                "skipped_checks":[{"check_id":"gpu_count", "skip_reason":"DEPENDENCY_SKIPPED"}]})).unwrap();
            let result = DsxHealthValidation
                .interpret(
                    context(&input, Path::new("/unused")),
                    AdapterOutput {
                        exit_code: Some(exit),
                        stdout: &stdout,
                        stdout_truncated: false,
                    },
                )
                .unwrap();
            assert_eq!(result.outcome, expected);
            assert_eq!(result.severity.as_deref(), severity);
            assert!(result.findings.is_empty());
        }
        let result = DsxHealthValidation
            .interpret(
                context(&input, Path::new("/unused")),
                AdapterOutput {
                    exit_code: Some(1),
                    stdout: br#"{"overall_severity":"CRITICAL"}"#,
                    stdout_truncated: false,
                },
            )
            .unwrap();
        assert_eq!(result.outcome, "fail");
    }

    #[test]
    fn invalid_output_or_process_failure_is_an_execution_error() {
        let input = serde_json::json!({});
        let context = context(&input, Path::new("/unused"));
        for stdout in [
            "not JSON",
            "{}",
            r#"["PASS"]"#,
            r#"{"overall_severity":null}"#,
            r#"{"overall_severity":"future"}"#,
        ] {
            assert!(
                DsxHealthValidation
                    .interpret(
                        context,
                        AdapterOutput {
                            exit_code: Some(0),
                            stdout: stdout.as_bytes(),
                            stdout_truncated: false
                        }
                    )
                    .is_err()
            );
        }
        for (exit, truncated) in [(Some(0), true), (Some(2), false), (None, false)] {
            assert!(
                DsxHealthValidation
                    .interpret(
                        context,
                        AdapterOutput {
                            exit_code: exit,
                            stdout: br#"{"overall_severity":"PASS"}"#,
                            stdout_truncated: truncated
                        }
                    )
                    .is_err()
            );
        }
    }
}
