/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use carbide_host_support::dpa_cmds::{DpaCommand, OpCode};
use carbide_host_support::registration;
use carbide_instrument::emit;
use carbide_uuid::machine::MachineId;
use cfg::{AutoDetect, Command, MlxAction, Mode, Options};
use chrono::{DateTime, Days, TimeDelta, Utc};
use clap::CommandFactory;
use libmlx::device::cmd::device::args::DeviceArgs;
use libmlx::device::cmd::device::cmds::handle as handle_mlx_device;
use libmlx::device::discovery::discover_device;
use libmlx::lockdown::cmd::cmds::handle_lockdown as handle_mlx_lockdown;
use once_cell::sync::Lazy;
use rpc::forge::ForgeAgentControlResponse;
use rpc::forge_agent_control_response::Action;
use rpc::protos::mlx_device::{
    FirmwareFlashReport as FirmwareFlashReportPb, LockStatus, MlxObservation, MlxObservationReport,
    PublishMlxObservationReportRequest,
};
use rpc::{
    ForgeScoutErrorReport, forge as rpc_forge, forge_agent_control_response as fac,
    scout_firmware_upgrade as sfu,
};
use scout::{CarbideClientError, CarbideClientResult};
use tokio::sync::RwLock;
use tryhard::{RetryFutureConfig, RetryPolicy};
use x509_parser::pem::parse_x509_pem;
use x509_parser::prelude::{FromDer, X509Certificate};

mod attestation;
mod cfg;
mod client;
mod deprovision;
mod discovery;
mod firmware_upgrade;
mod machine_validation;
mod metrics;
mod mlx_device;
mod platform;
mod register;
mod stream;
mod tpm;

struct DevEnv {
    in_qemu: bool,
}
static IN_QEMU_VM: Lazy<RwLock<DevEnv>> = Lazy::new(|| RwLock::new(DevEnv { in_qemu: false }));
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const REBOOT_COMPLETED_PATH: &str = "/tmp/reboot_completed";
const MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE: usize = 1500;
const CLOUD_INIT_OUTPUT_LOG: &str = "/var/log/cloud-init-output.log";

/// Listed explicitly so that a value we do not recognize is treated as unknown
/// rather than as a failure; cloud-init offers no stability guarantee here.
const CLOUD_INIT_STATUS_NOT_CLEAN: &[&str] =
    &["not run", "not started", "running", "error", "disabled"];

/// Backstop against `--wait` never returning, which would strand the machine in
/// discovery. The real bound on customization is cloud-init's own unit timeout,
/// so this is set well above any legitimate run rather than tuned to one.
const CLOUD_INIT_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

async fn check_if_running_in_qemu() {
    use tokio::process::Command;
    let output = match Command::new("systemd-detect-virt").output().await {
        Ok(s) => s,
        Err(_) => {
            // Not sure. But if above command is not present,
            // assume it real machine.
            return;
        }
    };

    if let Ok(x) = String::from_utf8(output.stdout)
        && x.trim() != "none"
    {
        IN_QEMU_VM.write().await.in_qemu = true;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), eyre::Report> {
    let config = Options::load();
    if config.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }

    // Purely local interrogation for troubleshooting.
    if matches!(config.subcmd, Some(Command::LldpNeighbors)) {
        let neighbors = carbide_host_support::lldp_collector::collect_lldp_neighbors().await?;
        println!("{neighbors:#?}");
        return Ok(());
    }

    check_if_running_in_qemu().await;

    carbide_host_support::init_logging("nico-scout")?;

    tracing::info!(
        mode = %config.mode,
        version_requested = config.version,
        "Running scout",
    );

    match config.mode {
        Mode::Service => run_as_service(&config).await?,
        Mode::Standalone => run_standalone(&config).await?,
    }
    Ok(())
}

async fn initial_setup(config: &Options) -> Result<(uuid::Uuid, MachineId), eyre::Report> {
    // we use the same retry params for both: retrying the discover_machine
    // call, as well as retrying the whole attestation sequence: discover_machine + attest_quote
    let retry = registration::DiscoveryRetry {
        secs: config.discovery_retry_secs,
        max: config.discovery_retries_max,
    };

    let (machine_id, interface_id) = match tryhard::retry_fn(|| {
        tracing::info!("Trying to register the machine");
        register::run(
            &config.api,
            config.root_ca.clone(),
            config.machine_interface_id,
            &retry,
            &config.tpm_path,
        )
    })
    .retries(retry.max)
    .custom_backoff(|_attempt, error: &CarbideClientError| {
        // we only want to retry if attestation has failed. In all other cases
        // just preserve the old behaviour by breaking from the retry loop
        tracing::error!(error = %error, "Failed to register machine");
        if !error
            .to_string()
            .to_lowercase()
            .contains("attestation failed")
        {
            tracing::info!("Not retrying registration as it is not an attestation error");
            RetryPolicy::Break
        } else {
            tracing::info!(retry_delay_seconds = retry.secs, "Retrying registration",);
            RetryPolicy::Delay(Duration::from_secs(retry.secs))
        }
    })
    .await
    {
        Ok(machine_id) => machine_id,
        Err(e) => {
            report_scout_error(config, None, config.machine_interface_id, &e).await?;
            return Err(e.into());
        }
    };

    if !Path::new(REBOOT_COMPLETED_PATH).exists() {
        discovery::rebooted(config, &machine_id).await?;
        let mut data_file = File::create(REBOOT_COMPLETED_PATH).expect("creation failed");
        data_file.write_all(format!("Reboot completed at {}", chrono::Utc::now()).as_bytes())?;
    }

    let machine_interface_id = if let Some(interface_id) = config.machine_interface_id {
        interface_id
    } else if let Some(interface_id) = interface_id {
        interface_id
    } else {
        return Err(eyre::eyre!(
            "machine_interface_id is unknown. can't continue"
        ));
    };

    Ok((machine_interface_id, machine_id))
}

async fn run_as_service(config: &Options) -> Result<(), eyre::Report> {
    // Stand up the metrics/scrape endpoint when it is configured. The meter
    // provider must stay alive for the lifetime of the service: dropping it
    // shuts down the Prometheus exporter. The endpoint is opt-in -- without
    // --metrics-listen-addr scout installs no meter and the counters below are
    // no-ops.
    let _metrics_guard = match config.metrics_listen_addr {
        Some(address) => {
            let metrics_setup =
                metrics_endpoint::new_metrics_setup("nico-scout", "forge-system", true)?;
            carbide_instrument::log_events::register(&metrics_setup.meter);
            let metrics_config = metrics_endpoint::MetricsEndpointConfig {
                address,
                registry: metrics_setup.registry,
                health_controller: Some(metrics_setup.health_controller),
                additional_prefix: None,
            };
            // The endpoint's /health and /ready report process liveness (the
            // default HealthController state), not scout readiness.
            tokio::spawn(async move {
                tracing::info!("Spawning metrics endpoint on {}", metrics_config.address);
                if let Err(e) = metrics_endpoint::run_metrics_endpoint(&metrics_config).await {
                    tracing::error!("Metrics endpoint error: {e}");
                }
            });
            Some(metrics_setup.meter_provider)
        }
        None => None,
    };

    // Blocks: scout must not describe a machine that is still being set up.
    // Ahead of registration so that a customization failure bad enough to break
    // registration is still reported; that is why ReportForgeScoutError permits
    // the Anonymous principal, since the client cert arrives in the
    // DiscoverMachine response.
    report_cloud_init_outcome(config).await;

    // Implement the logic to run as a service here
    let (machine_interface_id, machine_id) = initial_setup(config).await?;

    // set up a task to check once a day if certs are less than two days from expiry
    let client_cert = config.client_cert.clone();

    let mut next_certs_check_time = get_next_certs_check_datetime()?;

    // Do a one-time publish of MlxDeviceReport data at service
    // start time for now. This will eventually be folded into
    // an Action::MlxDevice feedback loop, but doing some preliminary
    // work for now to get at the data. This was originally part of the
    // common registration client, but I realized that was the wrong
    // place to put it: since this is going to be part of the Action
    // feedback loop, a more accurate place to run this would be after
    // initial_setup (and after registration is complete).
    match mlx_device::create_device_report_request(machine_id) {
        Ok(request) => match mlx_device::publish_mlx_device_report(config, request).await {
            Ok(response) => tracing::info!(?response, "received PublishMlxDeviceReportResponse",),
            Err(e) => emit(metrics::ScoutMlxOperationFailed::DeviceReportPublish {
                error: format!("{e:?}"),
            }),
        },
        Err(e) => emit(metrics::ScoutMlxOperationFailed::DeviceReportCreate {
            error: format!("{e:?}"),
        }),
    };

    let mut scout_stream_started = false;
    loop {
        if is_time_to_check_certs_expiry(next_certs_check_time) {
            next_certs_check_time = get_next_certs_check_datetime()?;
            tracing::info!(
                %next_certs_check_time,
                "Renewed next certificate check time",
            );

            if check_certs_validity(&client_cert)? {
                initial_setup(config).await?;
            }
        }
        let controller_response = match query_api_with_retries(config, &machine_id).await {
            Ok(action) => action,
            Err(e) => {
                report_scout_error(config, None, Some(machine_interface_id), &e).await?;
                rpc_forge::ForgeAgentControlResponse::noop()
            }
        };
        if let Some(action) = controller_response.action {
            let action_name = action.as_str_name();
            // Capture the action label before handle_action consumes `action`.
            let scout_action = metrics::ScoutAction::from(&action);
            let result = handle_action(action, &machine_id, machine_interface_id, config).await;
            emit(match result {
                Ok(()) => metrics::ScoutActionHandled::Ok {
                    action: scout_action,
                    action_name,
                },
                Err(error) => metrics::ScoutActionHandled::Error {
                    action: scout_action,
                    action_name,
                    error: error.to_string(),
                },
            });
        } else {
            tracing::warn!("API response did not contain an action, skipping.");
        }

        // Ensure the first scout API query has run before we establish
        // a Scout stream connection. There's no technical reason requiring
        // this, other than it seemed to make sense to do 1 control
        // request/response action flow before setting up any additional
        // scaffolding.
        if !scout_stream_started {
            scout_stream_started = true;
            stream::start_scout_stream(machine_id, config);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn run_standalone(config: &Options) -> Result<(), eyre::Report> {
    // Implement the logic for standalone mode here
    let subcmd: &Command = match &config.subcmd {
        None => {
            Options::command().print_long_help()?;
            std::process::exit(1);
        }
        Some(s) => match s {
            // The mlx-device subcommand doesn't need to be run with any
            // sort of API integration; it's intended purely for interrogating
            // the state of Mellanox devices on the machine, to see what
            // Carbide will see re: reporting, troubleshooting, etc. Just
            // run the command locally and exit.
            Command::Mlx(mlx) => match &mlx.action {
                // Then match on the specific action (Device or Lockdown)
                MlxAction::Device(mlx_device) => {
                    // The mlx device subcommand doesn't need to be run with any
                    // sort of API integration; it's intended purely for interrogating
                    // the state of Mellanox devices on the machine, to see what
                    // Carbide will see re: reporting, troubleshooting, etc. Just
                    // run the command locally and exit.
                    let device_args = DeviceArgs {
                        action: mlx_device.action.clone(),
                    };
                    handle_mlx_device(device_args).map_err(|e| eyre::eyre!("{e}"))?;
                    return Ok(());
                }
                MlxAction::Lockdown(mlx_lockdown) => {
                    handle_mlx_lockdown(mlx_lockdown.action.clone())
                        .map_err(|e| eyre::eyre!("{e}"))?;
                    return Ok(());
                }
            },
            _ => s,
        },
    };

    let (machine_interface_id, machine_id) = initial_setup(config).await?;

    //TODO Could be better; this for backward compatibility. Refactor required
    let controller_response = match query_api_with_retries(config, &machine_id).await {
        Ok(controller_response) => controller_response,
        Err(e) => {
            report_scout_error(config, None, Some(machine_interface_id), &e).await?;
            ForgeAgentControlResponse::noop()
        }
    };
    let action = match subcmd {
        Command::AutoDetect(AutoDetect { .. }) => {
            let Some(action) = controller_response.action else {
                tracing::warn!("ForgeAgentControlResponse from server has no action: ignoring");
                return Ok(());
            };
            action
        }
        Command::Deprovision(_) => Action::reset(),
        Command::Discovery(_) => Action::discovery(),
        Command::Reset(_) => Action::reset(),
        Command::Logerror(_) => Action::log_error(),
        Command::MachineValidation(data) => {
            fac::Action::MachineValidation(fac::MachineValidation {
                is_enabled: true,
                context: data.context.clone(),
                validation_id: Some(data.validataion_id),
                filter: Some(fac::MachineValidationFilter {
                    tags: Vec::new(),
                    allowed_tests: Vec::new(),
                    run_unverfied_tests: None,
                    contexts: None,
                }),
            })
        }
        // This will have already been caught above and
        // handled, but we need to have it here to make
        // sure we match everything. Maybe this could
        // log something.
        Command::Mlx(_) => return Ok(()),
        // Handled in main() before logging/API setup; kept for exhaustiveness.
        Command::LldpNeighbors => return Ok(()),
    };

    handle_action(action, &machine_id, machine_interface_id, config).await?;
    Ok(())
}

async fn handle_action(
    action: Action,
    machine_id: &MachineId,
    machine_interface_id: uuid::Uuid,
    config: &Options,
) -> Result<(), CarbideClientError> {
    match action {
        fac::Action::Discovery(_) => {
            // Discovery prep must not scrub storage. NVMe/HDD cleanup is owned by RESET so
            // cleanup status is reported to the API and retried through the cleanup state.
            deprovision::run_no_api(&config.tpm_path).await?;
            let retry = registration::DiscoveryRetry {
                secs: config.discovery_retry_secs,
                max: config.discovery_retries_max,
            };

            register::run(
                &config.api,
                config.root_ca.clone(),
                Some(machine_interface_id),
                &retry,
                &config.tpm_path,
            )
            .await?;

            discovery::completed(config, machine_id).await?;
        }
        fac::Action::Reset(_) => {
            deprovision::run(config, machine_id).await?;
        }
        fac::Action::Rebuild(_) => {
            unimplemented!("Rebuild not written yet");
        }
        fac::Action::Noop(_) => {}
        fac::Action::LogError(_) => logerror_to_carbide(config, machine_interface_id, None)
            .await
            // Propagate the failure so `carbide_scout_actions_total` records this
            // as `outcome = error`, not a silent success.
            .map_err(|e| {
                CarbideClientError::GenericError(format!("logerror_to_carbide failed: {e}"))
            })?,
        fac::Action::Retry(_) => {
            panic!(
                "Retrieved Retry action, which should be handled internally by query_api_with_retries"
            );
        }
        fac::Action::Measure(_) => {
            initial_setup(config).await.map_err(|e| {
                CarbideClientError::GenericError(format!(
                    "Could not perform attestation at the request of forge agent control: {e}"
                ))
            })?;
        }
        fac::Action::MachineValidation(machine_validation) => {
            tracing::info!("Machine validation");
            let id = machine_validation.validation_id.ok_or_else(|| {
                CarbideClientError::GenericError(
                    "machine validation action missing validation_id".to_string(),
                )
            })?;
            let machine_validation_filter = machine_validation
                .filter
                .map(Into::into)
                .unwrap_or_default();
            let ret = if machine_validation.is_enabled {
                match machine_validation::run(
                    config,
                    machine_id,
                    id,
                    machine_validation.context,
                    machine_validation_filter,
                )
                .await
                {
                    Ok(_) => {
                        tracing::info!("Machine validation completed");
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
            } else {
                Ok(())
            };
            let machine_validation_error = ret.as_ref().err().map(|err| err.to_string());
            machine_validation::completed(config, machine_id, &id, machine_validation_error)
                .await?;
            return ret;
        }
        fac::Action::MlxAction(mlx_action) => {
            handle_mlxreport_action(config, machine_id, mlx_action).await;
            return Ok(());
        }
        fac::Action::FirmwareUpgrade(firmware_upgrade) => {
            handle_firmware_upgrade_action(config, machine_id, firmware_upgrade.task).await?;
        }
    }
    Ok(())
}

async fn handle_firmware_upgrade_action(
    config: &Options,
    machine_id: &MachineId,
    task: Option<sfu::ScoutFirmwareUpgradeTask>,
) -> Result<(), CarbideClientError> {
    let task = task.ok_or_else(|| {
        CarbideClientError::GenericError("firmware upgrade action missing task".to_string())
    })?;

    let http_client = reqwest::Client::builder().no_proxy().build().map_err(|e| {
        CarbideClientError::GenericError(format!("failed to build HTTP client: {e}"))
    })?;

    tracing::info!(
        component_type = %task.component_type,
        target_version = %task.target_version,
        "[firmware_upgrade] received upgrade task",
    );

    let result = firmware_upgrade::handle_firmware_upgrade(&http_client, &task).await;

    tracing::info!(
        success = result.success,
        component_type = %task.component_type,
        target_version = %task.target_version,
        exit_code = result.exit_code,
        "[firmware_upgrade] upgrade finished",
    );

    report_firmware_upgrade_status(config, machine_id, task.upgrade_task_id, &result).await?;

    if !result.success {
        return Err(CarbideClientError::GenericError(format!(
            "firmware upgrade failed for component={} version={}: exit_code={} error={}",
            task.component_type, task.target_version, result.exit_code, result.error,
        )));
    }
    Ok(())
}

async fn report_firmware_upgrade_status(
    config: &Options,
    machine_id: &MachineId,
    upgrade_task_id: String,
    result: &firmware_upgrade::FirmwareUpgradeResult,
) -> Result<(), CarbideClientError> {
    let mut client = client::create_forge_client(config).await?;
    let request = tonic::Request::new(rpc_forge::ScoutFirmwareUpgradeStatusRequest {
        machine_id: Some(machine_id.try_into()?),
        success: result.success,
        exit_code: result.exit_code,
        stdout: truncate(&result.stdout, MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE),
        stderr: truncate(&result.stderr, MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE),
        error: truncate(&result.error, MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE),
        upgrade_task_id,
    });
    client.report_scout_firmware_upgrade_status(request).await?;
    Ok(())
}

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }

    if limit < 3 {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        return value[..end].to_string();
    }

    let mut end = limit - 2;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = String::with_capacity(end + 2);
    truncated.push_str(&value[..end]);
    truncated.push_str("..");
    truncated
}

// carbide sent us an Action::MlxReport command in response to our
// ForgeAgentControlRequest. Process the MlxReport action, which
// will involve doing configuration actions on our CIN NICs.
// We will send a publish_mlx_report request at the end to reflect
// the config actions we took.
async fn handle_mlxreport_action(
    config: &Options,
    machine_id: &MachineId,
    mlx_action: fac::MlxAction,
) {
    let commands = mlx_action
        .device_actions
        .iter()
        .filter_map(|device_action| match DpaCommand::try_from(device_action) {
            Ok(command) => Some((device_action.pci_name.clone(), command)),
            Err(e) => {
                emit(metrics::ScoutMlxReconciliationFailed::Decode {
                    pci_name: device_action.pci_name.clone(),
                    error: e,
                });
                None
            }
        })
        .collect();

    handle_mlxreport_commands(config, machine_id, commands).await;
}

async fn handle_mlxreport_commands(
    config: &Options,
    machine_id: &MachineId,
    commands: Vec<(String, DpaCommand<'static>)>,
) {
    let mut report = MlxObservationReport {
        machine_id: Some(*machine_id),
        timestamp: Some(Utc::now().into()),
        observations: Vec::new(),
    };

    for (dev_pci_name, dpa_cmd) in commands {
        if dev_pci_name.is_empty() {
            emit(metrics::ScoutMlxRequestRejected::Reconciliation {});
            continue;
        }

        let dev = match discover_device(&dev_pci_name) {
            Ok(d) => d,
            Err(s) => {
                emit(metrics::ScoutMlxReconciliationFailed::Discover {
                    pci_name: dev_pci_name,
                    error: s,
                });
                continue;
            }
        };

        match dpa_cmd.op {
            OpCode::Noop => (),
            OpCode::Lock { key } => match mlx_device::lock_device(&dev_pci_name, &key) {
                Ok(()) => {
                    let obs = MlxObservation {
                        device_info: Some(dev.into()),
                        lock_status: Some(LockStatus::Locked.into()),
                        profile_name: None,
                        profile_synced: None,
                        firmware_report: None,
                    };
                    report.observations.push(obs);
                }
                Err(e) => {
                    emit(metrics::ScoutMlxReconciliationFailed::Lock {
                        pci_name: dev_pci_name,
                        error: mlx_device::lockdown_error_context(&e, &key),
                    });
                }
            },
            // ApplyFirmware attempts to apply the provided FirmwareFlasherProfile
            // that it gets back from carbide-api. The profile *may* be None, which
            // could mean no firmware profile was found for the target Part Number
            // and PSID, or that carbide-api decided there was nothing to do here
            // (already at target version), or it wants to do a noop pass-through.
            // If None, scout reports a successful pass-through.
            //
            // Otherwise, scout constructs a FirmwareFlasher with new(..) validation
            // (device hw identity must match the FirmwareSpec), then calls apply()
            // which orchestrates: flash → reset → verify_image → verify_version.
            //
            // If flash() fails, scout does NOT send an observation, which causes
            // the DpaInterfaceController to remain in ApplyFirmware. On the next
            // poll, carbide-api will tell scout to try again.
            //
            // If flash() succeeds but a later step fails (reset, verify), scout
            // still sends the partial result so the API has visibility into what
            // happened. The state controller checks flashed && reset before
            // transitioning, so a partial failure stays in ApplyFirmware.
            OpCode::ApplyFirmware { profile } => {
                let firmware_report = match profile {
                    Some(profile) => mlx_device::apply_firmware(&dev_pci_name, &profile).await,
                    None => {
                        // No firmware profile was provided. Report a pass-through
                        // so the state controller transitions past ApplyFirmware.
                        tracing::info!(
                            device = %dev_pci_name,
                            "no firmware profile, skipping"
                        );
                        Some(FirmwareFlashReportPb {
                            flashed: true,
                            ..Default::default()
                        })
                    }
                };

                let obs = MlxObservation {
                    device_info: Some(dev.into()),
                    lock_status: None,
                    profile_name: None,
                    profile_synced: None,
                    firmware_report,
                };
                report.observations.push(obs);
            }
            OpCode::ApplyProfile { serialized_profile } => {
                let (profile_name, profile_synced) =
                    mlx_device::apply_profile(&dev_pci_name, serialized_profile);

                let obs = MlxObservation {
                    device_info: Some(dev.into()),
                    lock_status: None,
                    profile_name,
                    profile_synced,
                    firmware_report: None,
                };
                report.observations.push(obs);
            }
            OpCode::Unlock { key } => match mlx_device::unlock_device(&dev_pci_name, &key) {
                Ok(()) => {
                    let obs = MlxObservation {
                        device_info: Some(dev.into()),
                        lock_status: Some(LockStatus::Unlocked.into()),
                        profile_name: None,
                        profile_synced: None,
                        firmware_report: None,
                    };
                    report.observations.push(obs);
                }
                Err(e) => {
                    emit(metrics::ScoutMlxReconciliationFailed::Unlock {
                        pci_name: dev_pci_name,
                        error: mlx_device::lockdown_error_context(&e, &key),
                    });
                }
            },
        };
    }

    let req = PublishMlxObservationReportRequest {
        report: Some(report),
    };

    // Now send the report back to Carbide
    match mlx_device::publish_mlx_observation_report(config, req).await {
        Ok(_resp) => (),
        Err(e) => {
            emit(metrics::ScoutMlxOperationFailed::ObservationReportPublish {
                error: e.to_string(),
            });
        }
    }
}

// Return the tail of the cloud-init output log, up to max_bytes.
//
// A missing or empty log becomes the message rather than an error, because it
// is itself the thing worth reporting: cloud-init never ran.
fn get_log_str(max_bytes: usize) -> String {
    let text = match std::fs::read_to_string(CLOUD_INIT_OUTPUT_LOG) {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(
                path = CLOUD_INIT_OUTPUT_LOG,
                %error,
                "could not read the cloud-init output log"
            );
            return format!("no {CLOUD_INIT_OUTPUT_LOG} present, did cloud-init run?");
        }
    };

    let mut ret_str = String::new();
    for line in text.lines().rev() {
        let line_str = format!("{line}\n");
        ret_str.insert_str(0, &line_str);
        if ret_str.len() > max_bytes {
            break;
        }
    }

    if ret_str.is_empty() {
        return format!("{CLOUD_INIT_OUTPUT_LOG} is present but empty");
    }

    ret_str
}

/// How this boot's cloud-init customization went, as far as `cloud-init
/// status` will say. The failure cases are kept apart because they have
/// different owners: a bad site snippet, an image missing cloud-init, and a
/// wedged cloud-init are not diagnosed the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloudInitOutcome {
    Clean,
    NotClean,
    /// `cloud-init` could not be run at all.
    Unavailable,
    /// `cloud-init status --wait` did not return within [`CLOUD_INIT_WAIT_TIMEOUT`].
    TimedOut,
    /// A status value we do not recognize. Not treated as a failure: snippets
    /// are site-wide, so misreading one new string would have every machine in
    /// discovery report an error at once.
    Unknown,
}

/// Classifies `cloud-init status` from the exit code and the bare status value,
/// reading none of the fields cloud-init documents as unstable.
///
/// The exit code is primary (0 success, 1 crashed, 2 recoverable errors); the
/// status string catches the quiet cases, since `disabled` and `not run` both
/// exit 0 while meaning the snippets never applied.
fn classify_cloud_init_status(exit_code: Option<i32>, status_output: &str) -> CloudInitOutcome {
    // `--wait` writes progress dots as it blocks, so a real reply looks like
    // "..status: done". The prefix test stays anchored after stripping them so
    // that "extended_status:" cannot be mistaken for "status:".
    let status = status_output
        .lines()
        .find_map(|line| line.trim_start_matches(['.', ' ']).strip_prefix("status:"))
        .map(str::trim);

    match (exit_code, status) {
        (Some(0), Some("done")) => CloudInitOutcome::Clean,
        (Some(0), Some(status)) if CLOUD_INIT_STATUS_NOT_CLEAN.contains(&status) => {
            CloudInitOutcome::NotClean
        }
        // A non-zero exit is unambiguous whatever the status string says.
        (Some(code), _) if code != 0 => CloudInitOutcome::NotClean,
        // Exit 0 with a status we do not know, no status line at all, or a
        // `cloud-init status` killed by a signal.
        _ => CloudInitOutcome::Unknown,
    }
}

/// Asks cloud-init how this boot's site customization went, logging its full
/// answer locally and returning only a bounded verdict.
async fn get_cloud_init_outcome() -> CloudInitOutcome {
    use std::process::Stdio;

    use tokio::process::Command;

    // `--wait` is what orders scout after site customization. It cannot be done
    // with a systemd After=: cloud-final.service is ordered after
    // multi-user.target, so a unit in multi-user ordering after it forms a cycle
    // that systemd breaks by deleting scout's start job.
    //
    // kill_on_drop reaps the child when the timeout below drops the future.
    let child = Command::new("cloud-init")
        .args(["status", "--wait", "--long"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let child = match child {
        Ok(child) => child,
        Err(error) => {
            tracing::error!(%error, "could not run cloud-init status");
            return CloudInitOutcome::Unavailable;
        }
    };

    let output = match tokio::time::timeout(CLOUD_INIT_WAIT_TIMEOUT, child.wait_with_output()).await
    {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            tracing::error!(%error, "cloud-init status did not run to completion");
            return CloudInitOutcome::Unavailable;
        }
        Err(_elapsed) => {
            tracing::error!(
                timeout_seconds = CLOUD_INIT_WAIT_TIMEOUT.as_secs(),
                "cloud-init did not finish within the wait backstop; continuing without it"
            );
            return CloudInitOutcome::TimedOut;
        }
    };

    let status_output = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::info!(
        exit_code = ?output.status.code(),
        status_output = %status_output.trim(),
        stderr = %stderr.trim(),
        "cloud-init status",
    );

    classify_cloud_init_status(output.status.code(), &status_output)
}

/// Reports a cloud-init outcome that was not clean, so an ineffective snippet
/// is visible centrally rather than only on this host's console. Must never
/// fail the boot, so everything here is logged and swallowed.
async fn report_cloud_init_outcome(config: &Options) {
    match get_cloud_init_outcome().await {
        CloudInitOutcome::Clean => {
            tracing::info!("cloud-init reported a clean boot");
        }
        CloudInitOutcome::Unknown => {
            tracing::warn!(
                "cloud-init reported a status we do not recognize; treating it as unknown \
                 rather than as a failure and not reporting it"
            );
        }
        outcome @ (CloudInitOutcome::NotClean
        | CloudInitOutcome::Unavailable
        | CloudInitOutcome::TimedOut) => {
            let detail = match outcome {
                CloudInitOutcome::Unavailable => {
                    "cloud-init could not be run at all; this image may not have it installed"
                }
                CloudInitOutcome::TimedOut => {
                    "cloud-init did not finish within the wait backstop and may be wedged"
                }
                _ => "cloud-init did not complete cleanly",
            };
            let Some(machine_interface_id) = config.machine_interface_id else {
                tracing::error!(
                    detail,
                    "no machine interface ID to report the failure against"
                );
                return;
            };
            tracing::error!(detail, "reporting the cloud-init failure to the API");
            if let Err(error) =
                logerror_to_carbide(config, machine_interface_id, Some(detail)).await
            {
                tracing::error!(%error, "could not report the cloud-init failure to the API");
            }
        }
    }
}

// Send error string to carbide api to log, indicating that the cloud-init script failed.
// Very similar to report_scout_error below, but is run before discovery is done.
async fn logerror_to_carbide(
    config: &Options,
    machine_interface_id: uuid::Uuid,
    detail: Option<&str>,
) -> eyre::Result<()> {
    // The log tail alone cannot tell a missing cloud-init from a wedged one --
    // both yield the same "no log present" fallback -- so the caller's detail
    // leads. Its length comes out of MAX_ERR_MSG_SIZE, not on top of it.
    let err_str = match detail {
        Some(detail) => {
            let budget = (::rpc::MAX_ERR_MSG_SIZE as usize).saturating_sub(detail.len() + 1);
            format!("{detail}\n{}", get_log_str(budget))
        }
        None => get_log_str(::rpc::MAX_ERR_MSG_SIZE as usize),
    };
    let request: tonic::Request<ForgeScoutErrorReport> =
        tonic::Request::new(ForgeScoutErrorReport {
            machine_id: None,
            machine_interface_id: Some(machine_interface_id.into()),
            error: err_str,
        });

    let mut client = client::create_forge_client(config).await?;
    let _response = client.report_forge_scout_error(request).await?;

    Ok(())
}

async fn report_scout_error(
    config: &Options,
    machine_id: Option<MachineId>,
    machine_interface_id: Option<uuid::Uuid>,
    error: &impl std::error::Error,
) -> CarbideClientResult<()> {
    let request: tonic::Request<ForgeScoutErrorReport> =
        tonic::Request::new(ForgeScoutErrorReport {
            machine_id,
            machine_interface_id: machine_interface_id.map(|x| x.into()),
            error: format!("{error:#}"), // Alternate representation also prints inner errors
        });

    let mut client = client::create_forge_client(config).await?;
    let _response = client.report_forge_scout_error(request).await?.into_inner();
    Ok(())
}

/// Ask API if we need to do anything after discovery.
async fn query_api(
    config: &Options,
    machine_id: &MachineId,
    action_attempt: u64,
    query_attempt: u64,
) -> CarbideClientResult<rpc_forge::ForgeAgentControlResponse> {
    tracing::info!(
        action_attempt,
        query_attempt,
        "Sending ForgeAgentControlRequest",
    );
    let query = rpc_forge::ForgeAgentControlRequest {
        machine_id: Some(*machine_id),
    };
    let request = tonic::Request::new(query);
    let mut client = client::create_forge_client(config).await?;
    let response = client.forge_agent_control(request).await?.into_inner();
    let action_str = response
        .action
        .as_ref()
        .map(|a| a.as_str_name())
        .unwrap_or_default();

    tracing::info!(
        action_attempt,
        query_attempt,
        action = %action_str,
        "Received ForgeAgentControlResponse",
    );
    Ok(response)
}

async fn query_api_with_retries(
    config: &Options,
    machine_id: &MachineId,
) -> CarbideClientResult<rpc_forge::ForgeAgentControlResponse> {
    let mut action_attempt = 0;
    const MAX_RETRY_COUNT: u64 = 5;
    const RETRY_TIMER: u64 = 30;

    // The retry_config currently leverages the discovery_retry_*
    // flags passed in via the Scout command line, since this also
    // seems like a similar case where it should be persistent but
    // not aggressive. If there ends up being a desire to also have a
    // similar set of control_retry_* flags in the CLI, we can do
    // that (but trying to limit the number of flags if possible).
    let retry_config = RetryFutureConfig::new(config.discovery_retries_max)
        .fixed_backoff(Duration::from_secs(config.discovery_retry_secs))
        .on_retry(|_attempt, _next_delay, error: &CarbideClientError| {
            // We can't move the error, but CarbideClientError contains some results that are not clonable, so just do the format here
            let error = format!("{error}");
            async move { tracing::info!(error = %error, "ForgeAgentControlRequest failed") }
        });

    // State machine handler needs 1-2 cycles to update host_adminIP to leaf.
    // In case by the time, host comes up and IP is still not updated, let's wait.
    loop {
        // Depending on the forge_agent_control_response Action received
        // this entire loop may need to retry (as in, an Action::Retry was
        // received).
        //
        // BUT, that's in the case of the API call being successful (where
        // an Action is successfully returned). If the query_api attempt
        // itself fails, then IT needs to be retried as well, so query_api
        // also gets wrapped with a retry. Keep an inner attempt counter for
        // the purpose of tracing -- it seems helpful to know where in the
        // attempts thing sare.
        let mut query_attempt = 0u64;
        let controller_response = tryhard::retry_fn(|| {
            query_attempt += 1;
            query_api(config, machine_id, action_attempt, query_attempt)
        })
        .with_config(retry_config)
        .await?;

        action_attempt += 1;

        if !matches!(controller_response.action, Some(Action::Retry(_))) {
            return Ok(controller_response);
        }

        // +1 for the initial attempt which happens immediately
        if action_attempt == 1 + MAX_RETRY_COUNT {
            return Err(CarbideClientError::GenericError(format!(
                "Retrieved no viable Action for machine {} after {} secs",
                machine_id,
                MAX_RETRY_COUNT * RETRY_TIMER
            )));
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(RETRY_TIMER)).await;
    }
}

fn get_next_certs_check_datetime() -> CarbideClientResult<DateTime<Utc>> {
    let Some(next_certs_check_time) = Utc::now().checked_add_days(Days::new(1)) else {
        return Err(CarbideClientError::GenericError(
            "Could not obtain next certs check time".to_string(),
        ));
    };
    Ok(next_certs_check_time)
}

fn is_time_to_check_certs_expiry(next_check_time: DateTime<Utc>) -> bool {
    let now = Utc::now();
    let diff = next_check_time - now;
    if diff < TimeDelta::minutes(2) {
        tracing::info!(
            %now,
            %next_check_time,
            "Time to check certificate expiry",
        );
        return true;
    }
    false
}

fn check_certs_validity(client_cert_path: &str) -> CarbideClientResult<bool> {
    tracing::info!("Checking if client certs are going to expire soon ...");
    let mut ca_file = File::open(client_cert_path).map_err(CarbideClientError::StdIo)?;

    let mut ca_file_bytes: Vec<u8> = Vec::new();
    ca_file
        .read_to_end(&mut ca_file_bytes)
        .map_err(CarbideClientError::StdIo)?;

    let ca_file_bytes_der = {
        // convert pem to der to normalize
        let res = parse_x509_pem(&ca_file_bytes);
        match res {
            Ok((rem, pem)) => {
                if !rem.is_empty() && (pem.label != *"CERTIFICATE") {
                    return Err(CarbideClientError::GenericError(
                        "PEM certificate validation failed".to_string(),
                    ));
                }

                pem.contents
            }
            _ => {
                return Err(CarbideClientError::GenericError(
                    "Could not parse PEM certificate".to_string(),
                ));
            }
        }
    };

    // create the certificate
    let ca_cert = X509Certificate::from_der(&ca_file_bytes_der)
        .map_err(|e| CarbideClientError::GenericError(format!("Could not parse CA cert: {e}")))?
        .1;

    // if not after timestamp is less than two days away, initiate certs regen
    let not_after = ca_cert.validity.not_after.timestamp();
    if let Some(not_after_datetime) = DateTime::from_timestamp(not_after, 0) {
        let now = Utc::now();
        let diff = not_after_datetime - now;
        if diff < TimeDelta::days(2) {
            tracing::info!(
                %now,
                %not_after_datetime,
                "Certificate expires soon; triggering regeneration",
            );
            Ok(true)
        } else {
            tracing::info!(
                %now,
                %not_after_datetime,
                "Certificate does not expire soon; skipping regeneration",
            );
            Ok(false)
        }
    } else {
        Err(CarbideClientError::GenericError(format!(
            "Could not parse NotAfter timestamp: {not_after}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exit code decides, the status string catches the exit-0 cases that
    /// still mean nothing applied, and anything unfamiliar is unknown rather
    /// than a failure.
    #[test]
    fn cloud_init_status_is_classified_conservatively() {
        struct Case {
            scenario: &'static str,
            exit_code: Option<i32>,
            output: &'static str,
            expect: CloudInitOutcome,
        }

        let cases = [
            Case {
                scenario: "clean run",
                exit_code: Some(0),
                output: "status: done\nextended_status: done\nboot_status_code: enabled-by-kernel-cmdline\n",
                expect: CloudInitOutcome::Clean,
            },
            // Verbatim shape of a real reply: `--wait` prints a dot per poll,
            // so the first line does not begin with "status:".
            Case {
                scenario: "clean run behind the progress dots --wait prints",
                exit_code: Some(0),
                output: "..status: done\nextended_status: done\nboot_status_code: enabled-by-generator\nerrors: []\nrecoverable_errors: {}\n",
                expect: CloudInitOutcome::Clean,
            },
            Case {
                scenario: "dots on their own line",
                exit_code: Some(0),
                output: "....\nstatus: done\n",
                expect: CloudInitOutcome::Clean,
            },
            Case {
                scenario: "extended_status must never be read as status",
                exit_code: Some(0),
                output: "extended_status: degraded done\n",
                expect: CloudInitOutcome::Unknown,
            },
            Case {
                scenario: "degraded still reads done, and the exit code is what flags it",
                exit_code: Some(2),
                output: "status: done\nextended_status: degraded done\n",
                expect: CloudInitOutcome::NotClean,
            },
            Case {
                scenario: "crashed",
                exit_code: Some(1),
                output: "status: error\n",
                expect: CloudInitOutcome::NotClean,
            },
            Case {
                scenario: "disabled exits 0, so the status string is load-bearing",
                exit_code: Some(0),
                output: "status: disabled\n",
                expect: CloudInitOutcome::NotClean,
            },
            Case {
                scenario: "never ran",
                exit_code: Some(0),
                output: "status: not run\n",
                expect: CloudInitOutcome::NotClean,
            },
            Case {
                scenario: "a status value we have never seen is not a failure",
                exit_code: Some(0),
                output: "status: reticulating\n",
                expect: CloudInitOutcome::Unknown,
            },
            Case {
                scenario: "output with no status line at all",
                exit_code: Some(0),
                output: "something entirely different\n",
                expect: CloudInitOutcome::Unknown,
            },
            Case {
                scenario: "killed by a signal tells us nothing about cloud-init",
                exit_code: None,
                output: "",
                expect: CloudInitOutcome::Unknown,
            },
        ];

        for case in cases {
            assert_eq!(
                classify_cloud_init_status(case.exit_code, case.output),
                case.expect,
                "case '{}' failed",
                case.scenario,
            );
        }
    }

    /// A missing log is reported as a message, not raised as an error.
    #[test]
    fn missing_cloud_init_log_becomes_the_message() {
        if !std::path::Path::new(CLOUD_INIT_OUTPUT_LOG).exists() {
            assert_eq!(
                get_log_str(::rpc::MAX_ERR_MSG_SIZE as usize),
                format!("no {CLOUD_INIT_OUTPUT_LOG} present, did cloud-init run?"),
            );
        }
    }

    #[test]
    fn truncate_handles_short_long_and_utf8_values() {
        // Short values are preserved unchanged.
        assert_eq!(truncate("ok", MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE), "ok");

        // Long values are capped and marked as truncated.
        let value = "x".repeat(MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE + 100);
        let truncated = truncate(&value, MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE);
        assert_eq!(truncated.len(), MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE);
        assert!(truncated.ends_with(".."));

        // Multi-byte characters are never split.
        let value = "é".repeat(MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE);
        let truncated = truncate(&value, MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE);
        assert!(truncated.len() <= MAX_FIRMWARE_UPGRADE_STATUS_FIELD_SIZE);
        assert!(truncated.ends_with(".."));

        // Degenerate limits still respect the requested size.
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("hello", 1), "h");
        assert_eq!(truncate("é", 1), "");
    }
}
