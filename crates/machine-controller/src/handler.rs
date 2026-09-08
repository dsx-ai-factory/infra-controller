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

//! State Handler implementation for Machines

use std::collections::{HashMap, HashSet};
use std::mem::discriminant as enum_discr;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use attestation::{
    handle_spdm_attestation_failed_recovery, handle_spdm_poll_state, handle_spdm_trigger_state,
};
use carbide_credential_rotation::site_explorer_pause::{self, GateDecision};
use carbide_credential_rotation::{RotationStep, advance};
use carbide_firmware::{FirmwareConfig, FirmwareConfigSnapshot, FirmwareDownloader};
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::conv::{
    IntoLibredfish, IntoModel, machine_last_reboot_requested_mode,
};
use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineId};
use carbide_uuid::vpc::VpcId;
use chrono::{DateTime, Duration, Utc};
use config_version::{ConfigVersion, Versioned};
use db::DatabaseError;
use db::db_read::PgPoolReader;
use eyre::eyre;
use futures::TryFutureExt;
use futures_util::FutureExt;
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthProbeId, HealthReport, HealthReportApplyMode,
};
use itertools::Itertools;
use libredfish::model::oem::nvidia_dpu::HostPrivilegeLevel;
use libredfish::model::task::TaskState;
use libredfish::model::update_service::TransferProtocolType;
use libredfish::{Boot, EnabledDisabled, Redfish, RedfishError, SystemPowerControl};
use mac_address::MacAddress;
use machine_validation::{handle_machine_validation_requested, handle_machine_validation_state};
use measured_boot::records::MeasurementMachineState;
use model::DpuModel;
use model::dpa_interface::{DpaInterfaceControllerState, DpaInterfaceType, NewDpaInterface};
use model::expected_machine::ExpectedInterface;
use model::firmware::{Firmware, FirmwareComponentType, FirmwareEntry};
use model::instance::InstanceNetworkSyncStatus;
use model::instance::config::network::{
    DeviceLocator, InstanceInterfaceConfig, InterfaceFunctionId,
};
use model::instance::snapshot::InstanceSnapshot;
use model::instance::status::SyncState;
use model::instance::status::extension_service::{self, ExtensionServicesReadiness};
use model::machine::LockdownMode::{self, Enable};
use model::machine::infiniband::{IbConfigNotSyncedReason, ib_config_synced};
use model::machine::nvlink::nvlink_config_synced;
use model::machine::{
    AttestationMode, BomValidating, BomValidatingContext, CleanupContext, CleanupState,
    ConfigureAstraState, CreateBossVolumeContext, CreateBossVolumeState, DecommissioningState,
    DpuDiscoveringState, DpuDiscoveringStates, DpuInitNextStateResolver, DpuInitState,
    FactoryResetBmcState, FailureCause, FailureDetails, FailureSource,
    HostPlatformConfigurationState, HostReprovisionState, InitialResetPhase, InstallDpuOsState,
    InstanceNextStateResolver, InstanceState, LockdownInfo, LockdownState,
    MAX_FIRMWARE_UPGRADE_RETRIES, Machine, MachineLastRebootRequested,
    MachineLastRebootRequestedMode, MachineNextStateResolver, MachineState,
    MachineValidationContext, ManagedHostState, ManagedHostStateSnapshot, MeasuringState,
    NetworkConfigUpdateState, NextStateBFBSupport, PerformPowerOperation, PowerDrainState,
    PowerState, ReadyBootConfigPostLockAction, ReadyBootConfigState, ReprovisionState, RetryInfo,
    SecureEraseBossContext, SecureEraseBossState, SetBootOrderInfo, SetBootOrderState,
    SetSecureBootState, SpdmMeasuringState, StateMachineArea, UefiSetupInfo, UefiSetupState,
    UnlockHostState, ValidationState, dpf_based_dpu_provisioning_possible, get_display_ids,
};
use model::machine_boot_interface::MachineBootInterfaceTarget;
use model::power_manager::PowerHandlingOutcome;
use model::predicted_machine_interface::PredictedMachineInterface;
use model::resource_pool::common::CommonPools;
use model::site_explorer::ExploredEndpoint;
use sku::{handle_bom_validation_requested, handle_bom_validation_state};
use sqlx::PgConnection;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tokio::sync::Semaphore;
use tracing::instrument;
use version_compare::Cmp;

use crate::boot_interface::{BootInterfaceResolution, resolve_boot_interface};
use crate::config::{
    FirmwareGlobal, MachineStateHandlerSiteConfig, MachineValidationConfig, TimePeriod,
};
use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};
use crate::dpf::DpfOperations;
use crate::handler::firmware_artifact::ResolvedFirmwareArtifactSource;
use crate::health_report::{
    create_host_update_health_report_dpufw, create_host_update_health_report_hostfw,
};
use crate::redfish::{
    did_dpu_finish_booting, host_power_control, host_power_control_with_location,
};
use crate::{MeasuringOutcome, get_measuring_prerequisites, handle_measuring_state};

pub mod attestation;
mod bios_config;
mod boot_interface_observation;
mod decommissioning;
mod dpf;
mod dpu_action_handler;
mod dpu_uefi_rotation;
mod extension_services;
mod factory_reset;
mod firmware_artifact;
mod gb200_host_interface;
mod helpers;
mod host_boot_config;
mod host_uefi_rotation;
mod machine_validation;
mod maintenance;
mod nic_lockdown_rotation;
mod power;
mod rotation;
mod sku;
#[cfg(test)]
mod test_machine_setup;

use bios_config::handle_bios_setup_failed_recovery;
use extension_services::{cleanup_terminated_extension_services, get_extension_services_status};
use helpers::{
    DpuDiscoveringStateHelper, DpuInitStateHelper, ManagedHostStateHelper, NextState,
    ReprovisionStateHelper, all_equal,
};
use host_boot_config::{
    HostBootConfigCheckOutcome, HostBootConfigDecision, HostBootConfigDpuFreshness,
    HostBootConfigOutcome, HostBootConfigStage, check_host_boot_config, decide_host_boot_config,
    initial_set_boot_order_info, inspect_host_boot_config, run_host_boot_config_stage,
    should_skip_boot_order_remediation,
};
use state_controller::db_write_batch::DbWriteBatch;

use crate::config::{BomValidationConfig, PowerManagerOptions};
use crate::rpc::scout_firmware_upgrade::{FileArtifact, ScoutFirmwareUpgradeTask};
use crate::write_ops::MachineWriteOp;

// We can't use http::StatusCode because libredfish has a newer version
const NOT_FOUND: u16 = 404;

#[cfg(not(test))]
pub const MAX_NEW_FIRMWARE_REPORTED_RESET_RETRIES: u32 = 5;

#[cfg(test)]
pub const MAX_NEW_FIRMWARE_REPORTED_RESET_RETRIES: u32 = 2; // Faster for tests

/// A failed host firmware upgrade is being retried. `attempt` is the retry
/// about to run (1-based), out of a budget of [`MAX_FIRMWARE_UPGRADE_RETRIES`].
/// Machines whose budget ran out are visible on the
/// `carbide_exhausted_reprovision_retry_count` gauge instead.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "host_firmware_upgrade_retried",
    metric_name = "carbide_host_reprovision_retries_total",
    component = "machine-controller",
    log = info,
    metric = counter,
    message = "Retrying the host firmware upgrade",
    describe = "Number of times a failed host firmware upgrade was retried during host \
                reprovisioning"
)]
struct HostFirmwareUpgradeRetried {
    #[context]
    machine_id: MachineId,
    #[context]
    attempt: u32,
    #[context]
    error: String,
}

// Compute the API-side deadline for a scout firmware upgrade from scout's
// timeout envelope: fixed script download timeout, script execution timeout,
// one artifact download timeout per file artifact, and report/slack time.
// Saturating arithmetic prevents overflow from malformed configs, and the
// final cap prevents the API from waiting indefinitely for an absurd task.
fn scout_firmware_upgrade_deadline(
    started_at: DateTime<Utc>,
    execution_timeout_seconds: u32,
    artifact_download_timeout_seconds: u32,
    file_artifact_count: usize,
) -> DateTime<Utc> {
    // Must match crates/scout/src/firmware_upgrade.rs::SCRIPT_DOWNLOAD_TIMEOUT.
    const SCRIPT_DOWNLOAD_TIMEOUT_SECONDS: i64 = 30;
    const DEADLINE_SLACK: Duration = Duration::minutes(30);
    const MAX_DEADLINE_DURATION_SECONDS: i64 = 5 * 60 * 60;

    let artifact_download_seconds = i64::from(artifact_download_timeout_seconds)
        .saturating_mul(i64::try_from(file_artifact_count).unwrap_or(i64::MAX));
    let deadline_seconds = SCRIPT_DOWNLOAD_TIMEOUT_SECONDS
        .saturating_add(i64::from(execution_timeout_seconds))
        .saturating_add(artifact_download_seconds)
        .saturating_add(DEADLINE_SLACK.num_seconds())
        .min(MAX_DEADLINE_DURATION_SECONDS);

    started_at + Duration::seconds(deadline_seconds)
}

/// Reachability params to check if DPU is up or not.
#[derive(Copy, Clone, Debug)]
pub struct ReachabilityParams {
    pub dpu_wait_time: chrono::Duration,
    pub power_down_wait: chrono::Duration,
    pub failure_retry_time: chrono::Duration,
    pub scout_reporting_timeout: chrono::Duration,
    pub waiting_for_measurements_timeout: chrono::Duration,
    pub uefi_boot_wait: chrono::Duration,
}

/// Parameters used by the HostStateMachineHandler.
#[derive(Clone, Debug)]
pub struct HostHandlerParams {
    pub attestation_enabled: bool,
    pub reachability_params: ReachabilityParams,
    pub machine_validation_config: MachineValidationConfig,
    pub bom_validation: BomValidationConfig,
}

/// Parameters used by the Power config.
#[derive(Clone, Debug)]
pub struct PowerOptionConfig {
    pub enabled: bool,
    pub next_try_duration_on_success: chrono::TimeDelta,
    pub next_try_duration_on_failure: chrono::TimeDelta,
    pub wait_duration_until_host_reboot: chrono::TimeDelta,
}

impl From<PowerManagerOptions> for PowerOptionConfig {
    fn from(options: PowerManagerOptions) -> Self {
        Self {
            enabled: options.enabled,
            next_try_duration_on_success: options.next_try_duration_on_success,
            next_try_duration_on_failure: options.next_try_duration_on_failure,
            wait_duration_until_host_reboot: options.wait_duration_until_host_reboot,
        }
    }
}

/// The actual Machine State handler
#[derive(Debug, Clone)]
pub struct MachineStateHandler {
    host_handler: HostMachineStateHandler,
    pub dpu_handler: DpuMachineStateHandler,
    instance_handler: InstanceStateHandler,
    dpu_up_threshold: chrono::Duration,
    /// Reachability params to check if DPU is up or not
    reachability_params: ReachabilityParams,
    host_upgrade: Arc<HostUpgradeState>,
    power_options_config: PowerOptionConfig,
    enable_secure_boot: bool,
}

pub struct MachineStateHandlerBuilder {
    dpu_up_threshold: chrono::Duration,
    dpu_nic_firmware_initial_update_enabled: bool,
    // TODO: Cleanup needed for this flag.
    dpu_nic_firmware_reprovision_update_enabled: bool,
    hardware_models: Option<FirmwareConfig>,
    no_firmware_update_reset_retries: bool,
    reachability_params: ReachabilityParams,
    firmware_downloader: Option<FirmwareDownloader>,
    attestation_enabled: bool,
    upload_limiter: Option<Arc<Semaphore>>,
    machine_validation_config: MachineValidationConfig,
    common_pools: Option<Arc<CommonPools>>,
    bom_validation: BomValidationConfig,
    instance_autoreboot_period: Option<TimePeriod>,
    credential_reader: Option<Arc<dyn CredentialReader>>,
    power_options_config: PowerOptionConfig,
    enable_secure_boot: bool,
    hgx_bmc_gpu_reboot_delay: chrono::Duration,
    dpf_sdk: Option<Arc<dyn DpfOperations>>,
}

impl MachineStateHandlerBuilder {
    pub fn builder() -> Self {
        Self {
            dpu_up_threshold: chrono::Duration::minutes(5),
            dpu_nic_firmware_initial_update_enabled: true,
            dpu_nic_firmware_reprovision_update_enabled: true,
            hardware_models: None,
            reachability_params: ReachabilityParams {
                dpu_wait_time: chrono::Duration::zero(),
                power_down_wait: chrono::Duration::zero(),
                failure_retry_time: chrono::Duration::zero(),
                scout_reporting_timeout: chrono::Duration::zero(),
                waiting_for_measurements_timeout: chrono::Duration::zero(),
                uefi_boot_wait: chrono::Duration::zero(),
            },
            firmware_downloader: None,
            no_firmware_update_reset_retries: false,
            attestation_enabled: false,
            upload_limiter: None,
            machine_validation_config: MachineValidationConfig {
                enabled: true,
                ..MachineValidationConfig::default()
            },
            common_pools: None,
            bom_validation: BomValidationConfig::default(),
            instance_autoreboot_period: None,
            credential_reader: None,
            power_options_config: PowerOptionConfig {
                enabled: true,
                next_try_duration_on_success: chrono::Duration::minutes(0),
                next_try_duration_on_failure: chrono::Duration::minutes(0),
                wait_duration_until_host_reboot: chrono::Duration::minutes(0),
            },
            enable_secure_boot: false,
            hgx_bmc_gpu_reboot_delay: chrono::Duration::seconds(30),
            dpf_sdk: None,
        }
    }

    pub fn dpf_sdk(mut self, dpf_sdk: Option<Arc<dyn DpfOperations>>) -> Self {
        self.dpf_sdk = dpf_sdk;
        self
    }

    pub fn credential_reader(mut self, credential_reader: Arc<dyn CredentialReader>) -> Self {
        self.credential_reader = Some(credential_reader);
        self
    }
    pub fn dpu_up_threshold(mut self, dpu_up_threshold: chrono::Duration) -> Self {
        self.dpu_up_threshold = dpu_up_threshold;
        self
    }

    #[cfg(feature = "test-support")]
    pub fn dpu_nic_firmware_initial_update_enabled(
        mut self,
        dpu_nic_firmware_initial_update_enabled: bool,
    ) -> Self {
        self.dpu_nic_firmware_initial_update_enabled = dpu_nic_firmware_initial_update_enabled;
        self
    }

    pub fn dpu_nic_firmware_reprovision_update_enabled(
        mut self,
        dpu_nic_firmware_reprovision_update_enabled: bool,
    ) -> Self {
        self.dpu_nic_firmware_reprovision_update_enabled =
            dpu_nic_firmware_reprovision_update_enabled;
        self
    }

    #[cfg(feature = "test-support")]
    pub fn reachability_params(mut self, reachability_params: ReachabilityParams) -> Self {
        self.reachability_params = reachability_params;
        self
    }

    pub fn dpu_wait_time(mut self, dpu_wait_time: chrono::Duration) -> Self {
        self.reachability_params.dpu_wait_time = dpu_wait_time;
        self
    }

    pub fn dpu_enable_secure_boot(mut self, dpu_enable_secure_boot: bool) -> Self {
        self.enable_secure_boot = dpu_enable_secure_boot;
        self
    }

    pub fn power_down_wait(mut self, power_down_wait: chrono::Duration) -> Self {
        self.reachability_params.power_down_wait = power_down_wait;
        self
    }

    pub fn failure_retry_time(mut self, failure_retry_time: chrono::Duration) -> Self {
        self.reachability_params.failure_retry_time = failure_retry_time;
        self
    }

    pub fn scout_reporting_timeout(mut self, scout_reporting_timeout: chrono::Duration) -> Self {
        self.reachability_params.scout_reporting_timeout = scout_reporting_timeout;
        self
    }

    pub fn waiting_for_measurements_timeout(
        mut self,
        waiting_for_measurements_timeout: chrono::Duration,
    ) -> Self {
        self.reachability_params.waiting_for_measurements_timeout =
            waiting_for_measurements_timeout;
        self
    }

    pub fn uefi_boot_wait(mut self, uefi_boot_wait: chrono::Duration) -> Self {
        self.reachability_params.uefi_boot_wait = uefi_boot_wait;
        self
    }

    pub fn hardware_models(mut self, hardware_models: FirmwareConfig) -> Self {
        self.hardware_models = Some(hardware_models);
        self
    }

    pub fn firmware_downloader(mut self, firmware_downloader: &FirmwareDownloader) -> Self {
        self.firmware_downloader = Some(firmware_downloader.clone());
        self
    }

    pub fn attestation_enabled(mut self, attestation_enabled: bool) -> Self {
        self.attestation_enabled = attestation_enabled;
        self
    }

    pub fn upload_limiter(mut self, upload_limiter: Arc<Semaphore>) -> Self {
        self.upload_limiter = Some(upload_limiter);
        self
    }

    pub fn machine_validation_config(
        mut self,
        machine_validation_config: MachineValidationConfig,
    ) -> Self {
        self.machine_validation_config = machine_validation_config;
        self
    }

    pub fn common_pools(mut self, common_pools: Arc<CommonPools>) -> Self {
        self.common_pools = Some(common_pools);
        self
    }

    pub fn bom_validation(mut self, bom_validation: BomValidationConfig) -> Self {
        self.bom_validation = bom_validation;
        self
    }

    pub fn no_firmware_update_reset_retries(
        mut self,
        no_firmware_update_reset_retries: bool,
    ) -> Self {
        self.no_firmware_update_reset_retries = no_firmware_update_reset_retries;
        self
    }

    pub fn instance_autoreboot_period(mut self, period: Option<TimePeriod>) -> Self {
        self.instance_autoreboot_period = period;
        self
    }

    pub fn power_options_config(mut self, config: PowerOptionConfig) -> Self {
        self.power_options_config = config;
        self
    }

    pub fn build(self) -> MachineStateHandler {
        MachineStateHandler::new(self)
    }
}

impl MachineStateHandler {
    fn new(builder: MachineStateHandlerBuilder) -> Self {
        let host_upgrade = Arc::new(HostUpgradeState {
            parsed_hosts: Arc::new(builder.hardware_models.clone().unwrap_or_default()),
            downloader: builder.firmware_downloader.unwrap_or_default(),
            upload_limiter: builder
                .upload_limiter
                .unwrap_or(Arc::new(Semaphore::new(5))),
            no_firmware_update_reset_retries: builder.no_firmware_update_reset_retries,
            instance_autoreboot_period: builder.instance_autoreboot_period,
            upgrade_script_state: Default::default(),
            credential_reader: builder.credential_reader,
            async_firmware_uploader: Arc::new(Default::default()),
            hgx_bmc_gpu_reboot_delay: builder
                .hgx_bmc_gpu_reboot_delay
                .to_std()
                .unwrap_or(tokio::time::Duration::from_secs(30)),
        });
        MachineStateHandler {
            dpu_up_threshold: builder.dpu_up_threshold,
            host_handler: HostMachineStateHandler::new(HostHandlerParams {
                attestation_enabled: builder.attestation_enabled,
                reachability_params: builder.reachability_params,
                machine_validation_config: builder.machine_validation_config,
                bom_validation: builder.bom_validation,
            }),
            dpu_handler: DpuMachineStateHandler::new(
                builder.dpu_nic_firmware_initial_update_enabled,
                builder.hardware_models.clone().unwrap_or_default(),
                builder.reachability_params,
                builder.enable_secure_boot,
                builder.dpf_sdk.clone(),
            ),
            instance_handler: InstanceStateHandler::new(
                builder.reachability_params,
                builder.common_pools,
                host_upgrade.clone(),
                builder.hardware_models.clone().unwrap_or_default(),
                builder.enable_secure_boot,
                builder.dpf_sdk.clone(),
            ),
            reachability_params: builder.reachability_params,
            host_upgrade,
            power_options_config: builder.power_options_config,
            enable_secure_boot: builder.enable_secure_boot,
        }
    }

    fn record_metrics(
        &self,
        state: &mut ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) {
        for dpu_snapshot in state.dpu_snapshots.iter() {
            let fw_version = dpu_snapshot
                .status
                .hardware_info
                .as_ref()
                .and_then(|hi| hi.dpu_info.as_ref().map(|di| di.firmware_version.clone()));
            if let Some(fw_version) = fw_version {
                *ctx.metrics
                    .dpu_firmware_versions
                    .entry(fw_version)
                    .or_default() += 1;
            }

            for mut component in dpu_snapshot
                .status
                .inventory
                .as_ref()
                .map(|i| i.components.clone())
                .unwrap_or_default()
            {
                // Remove the URL field for metrics purposes. We don't want to report different metrics
                // just because the URL field in components differ. Only name and version are important
                component.url = String::new();
                *ctx.metrics
                    .machine_inventory_component_versions
                    .entry(component)
                    .or_default() += 1;
            }

            // Update DPU network health Prometheus metrics
            ctx.metrics.dpus_healthy += if dpu_snapshot
                .dpu_agent_health_report()
                .map(|health| health.alerts.is_empty())
                .unwrap_or(false)
            {
                1
            } else {
                0
            };
            if let Some(report) = dpu_snapshot.dpu_agent_health_report() {
                for alert in report.alerts.iter() {
                    *ctx.metrics
                        .dpu_health_probe_alerts
                        .entry((alert.id.clone(), alert.target.clone()))
                        .or_default() += 1;
                }
            }
            if let Some(observation) = dpu_snapshot.network_status_observation.as_ref() {
                if let Some(agent_version) = observation.agent_version.as_ref() {
                    *ctx.metrics
                        .agent_versions
                        .entry(agent_version.clone())
                        .or_default() += 1;
                }
                if Utc::now().signed_duration_since(observation.observed_at)
                    <= self.dpu_up_threshold
                {
                    ctx.metrics.dpus_up += 1;
                }

                *ctx.metrics
                    .client_certificate_expiry
                    .entry(observation.machine_id.to_string())
                    .or_default() = observation.client_certificate_expiry;
            }
        }

        ctx.metrics.is_usable_as_instance = state.is_usable_as_instance(false).is_ok();
        ctx.metrics.num_gpus = state
            .host_snapshot
            .status
            .hardware_info
            .as_ref()
            .map(|info| info.gpus.len())
            .unwrap_or_default();
        ctx.metrics.in_use_by_tenant = state
            .instance
            .as_ref()
            .map(|instance| instance.config.tenant.tenant_organization_id.clone());
        ctx.metrics.is_host_bios_password_set =
            state.host_snapshot.bios_password_set_time.is_some();
        ctx.metrics.sku = state.host_snapshot.config.hw_sku.clone();
        ctx.metrics.sku_device_type = state.host_snapshot.status.hw_sku_device_type.clone();

        // Note that DPU alerts may be suppressed (classifications removed) in the aggregate health report.
        ctx.metrics.health.populate(
            state.host_snapshot.id.to_string(),
            &state.aggregate_health,
            &state.host_snapshot.health_reports,
        );

        // Feed the per-object health classification metric. The registry filters
        // to the opted-in classifications and emits a series labeled with this
        // host's id; emitting an empty set clears any prior series once the host
        // becomes healthy.
        let in_use = ctx.metrics.in_use_by_tenant.is_some();
        ctx.services.per_object_metrics_registry.record(
            "machine",
            &state.host_snapshot.id.to_string(),
            &ctx.metrics.health.health_alert_classifications,
            vec![opentelemetry::KeyValue::new("in_use", in_use.to_string())],
        );
    }

    fn record_health_history(
        &self,
        mh_snapshot: &mut ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) {
        ctx.pending_db_writes
            .push(MachineWriteOp::PersistMachineHealthHistory {
                machine_id: mh_snapshot.host_snapshot.id,
                health_report: mh_snapshot.aggregate_health.clone(),
            })
    }

    async fn clear_dpu_reprovision(
        mh_snaphost: &ManagedHostStateSnapshot,
        txn: &mut PgConnection,
    ) -> Result<(), StateHandlerError> {
        db::machine::remove_health_report(
            txn,
            &mh_snaphost.host_snapshot.id,
            health_report::HealthReportApplyMode::Merge,
            model::machine_update_module::HOST_UPDATE_HEALTH_REPORT_SOURCE,
        )
        .await?;

        for dpu_snapshot in &mh_snaphost.dpu_snapshots {
            db::machine::clear_dpu_reprovisioning_request(txn, &dpu_snapshot.id, false).await?;
        }

        Ok(())
    }

    async fn clear_scout_timeout_alert(
        txn: &mut PgConnection,
        host_machine_id: &MachineId,
    ) -> Result<(), StateHandlerError> {
        db::machine::remove_health_report(
            txn,
            host_machine_id,
            health_report::HealthReportApplyMode::Merge,
            "scout",
        )
        .await?;
        Ok(())
    }

    async fn clear_host_reprovision(
        mh_snaphost: &ManagedHostStateSnapshot,
        txn: &mut PgConnection,
    ) -> Result<(), StateHandlerError> {
        // Host fw update health override is not set yet. It is done when host re-provisioning is
        // started in state handler.
        db::host_machine_update::clear_host_reprovisioning_request(
            txn,
            &mh_snaphost.host_snapshot.id,
        )
        .await?;
        Ok(())
    }

    async fn clear_host_update_alert_and_reprov(
        mh_snaphost: &ManagedHostStateSnapshot,
        txn: &mut PgConnection,
    ) -> Result<(), StateHandlerError> {
        // Clear DPU reprovision
        Self::clear_dpu_reprovision(mh_snaphost, txn).await?;

        // Clear host reprovision
        Self::clear_host_reprovision(mh_snaphost, txn).await
    }

    async fn attempt_state_transition(
        &self,
        host_machine_id: &MachineId,
        mh_snapshot: &mut ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let mh_state = mh_snapshot.managed_state.clone();

        // If it's been more than 5 minutes since DPU reported status, consider it unhealthy
        for dpu_snapshot in &mh_snapshot.dpu_snapshots {
            if let Some(dpu_health) = dpu_snapshot.dpu_agent_health_report() {
                if !dpu_health.alerts.is_empty() {
                    continue;
                }
                if let Some(observation) = &dpu_snapshot.network_status_observation {
                    let observed_at = observation.observed_at;
                    let since_last_seen = Utc::now().signed_duration_since(observed_at);
                    if since_last_seen > self.dpu_up_threshold {
                        let message = format!("Last seen over {} ago", self.dpu_up_threshold);
                        let dpu_machine_id = dpu_snapshot.dpu_machine_id()?;
                        let dpu_machine_id = &dpu_machine_id;
                        let health_report = health_report::HealthReport::heartbeat_timeout(
                            health_report::HealthReport::DPU_AGENT_SOURCE.to_string(),
                            health_report::HealthReport::DPU_AGENT_SOURCE.to_string(),
                            message,
                            true,
                            false,
                        );

                        let mut txn = ctx.services.db_pool.begin().await?;
                        db::machine::update_dpu_agent_health_report(
                            &mut txn,
                            dpu_machine_id,
                            &health_report,
                        )
                        .await?;

                        tracing::warn!(
                        host_machine_id = %host_machine_id,
                        dpu_machine_id = %dpu_machine_id,
                        last_seen = %observed_at,
                        "DPU is not sending network status observations, marking unhealthy");
                        // The next iteration will run with the now unhealthy network
                        return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
                    }
                }
            }
        }

        // `ReadyBootConfigState::Prepare` must consume a snapshot with current
        // DPU `MachineNetworkStatusObservation` values before
        // `handle_restart_verification` can issue another reboot. A pending
        // `ReadyBootConfigPostLockAction`, or a later state with stale DPU
        // network status, must complete `LockHost` first.
        let active_boot_config = match &mh_state {
            ManagedHostState::BootConfiguring {
                boot_config_state: ReadyBootConfigState::Failed { .. },
                ..
            } => None,
            ManagedHostState::BootConfiguring {
                boot_config_state, ..
            } => Some(boot_config_state),
            _ => None,
        };
        let defer_restart_verification = active_boot_config.is_some_and(|boot_config_state| {
            matches!(
                boot_config_state,
                ReadyBootConfigState::Prepare
                    | ReadyBootConfigState::LockHost {
                        post_lock_action: Some(_),
                    }
            ) || !mh_snapshot.managed_host_network_config_version_synced()
        });

        if !defer_restart_verification
            && let Some(restart_transition) = handle_restart_verification(mh_snapshot, ctx).await?
        {
            return Ok(restart_transition);
        }

        if dpu_reprovisioning_needed(&mh_snapshot.dpu_snapshots) {
            // Reprovision is started and user requested for restart of reprovision.
            let restart_reprov = can_restart_reprovision(
                &mh_snapshot.dpu_snapshots,
                mh_snapshot.host_snapshot.state.version,
            );
            if restart_reprov
                && let Some(next_state) = self
                    .start_dpu_reprovision(&mh_state, mh_snapshot, ctx, host_machine_id)
                    .await?
            {
                return Ok(StateHandlerOutcome::transition(next_state));
            }
        }

        // Don't update failed state failure cause everytime. Record first failure cause only,
        // otherwise first failure cause will be overwritten.
        if !matches!(mh_state, ManagedHostState::Failed { .. })
            && let Some((machine_id, details)) = get_failed_state(mh_snapshot)
        {
            let already_relocking_machine_failure = matches!(
                &mh_state,
                ManagedHostState::BootConfiguring {
                    boot_config_state:
                        ReadyBootConfigState::LockHost {
                            post_lock_action:
                                Some(ReadyBootConfigPostLockAction::Machine {
                                    machine_id: pending_machine_id,
                                    details: pending_details,
                                }),
                        },
                    ..
                } if *pending_machine_id == machine_id && *pending_details == details
            );

            // Boot reconciliation may have opened lockdown. Preserve a newly
            // observed host or DPU failure, but restore lockdown before moving
            // the managed host into its terminal state.
            if already_relocking_machine_failure {
                tracing::info!(
                    host_machine_id = %mh_snapshot.host_snapshot.id,
                    failed_machine_id = %machine_id,
                    "Deferring global failure transition until boot reconciliation restores lockdown",
                );
            } else {
                tracing::error!(
                    host_id = %mh_snapshot.host_snapshot.id,
                    dpu_ids = %get_display_ids(&mh_snapshot.dpu_snapshots),
                    failed_machine_id = %machine_id,
                    ?details,
                    "ManagedHost is moved to Failed state",
                );
                let next_state = match &mh_state {
                    ManagedHostState::Assigned { .. } => ManagedHostState::Assigned {
                        instance_state: InstanceState::Failed {
                            details,
                            machine_id,
                        },
                    },
                    ManagedHostState::BootConfiguring {
                        desired_version,
                        desired_boot_interface,
                        post_lock_verification_retry_count,
                        boot_config_state,
                    } if !matches!(boot_config_state, ReadyBootConfigState::Failed { .. }) => {
                        ready_boot_config_locking(
                            Versioned {
                                value: desired_boot_interface.clone(),
                                version: *desired_version,
                            },
                            *post_lock_verification_retry_count,
                            Some(ReadyBootConfigPostLockAction::Machine {
                                machine_id,
                                details,
                            }),
                        )
                    }
                    _ => ManagedHostState::Failed {
                        details,
                        machine_id,
                        retry_count: 0,
                    },
                };
                return Ok(StateHandlerOutcome::transition(next_state));
            }
        }

        match &mh_state {
            ManagedHostState::ConfigureAstra {
                configure_astra_state,
            } => {
                match configure_astra_state {
                    ConfigureAstraState::EnableNics => {
                        let powercycle_needed =
                            self.enable_astra_all_nics(mh_snapshot, ctx).await?;
                        tracing::info!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            powercycle_needed,
                            "ConfigureAstra state: EnableNics"
                        );
                        if powercycle_needed {
                            if let Err(e) = handler_host_power_control(
                                mh_snapshot,
                                ctx,
                                SystemPowerControl::ACPowercycle,
                            )
                            .await
                            {
                                tracing::error!(
                                    machine_id = %mh_snapshot.host_snapshot.id,
                                    error = %e,
                                    "ConfigureAstra failed to ACPowercycle host"
                                );
                                return Err(e);
                            }
                            tracing::info!(
                                "ConfigureAstra called handler_host_power_control with ACPowercycle"
                            );
                            return Ok(StateHandlerOutcome::transition(
                                ManagedHostState::ConfigureAstra {
                                    configure_astra_state:
                                        ConfigureAstraState::WaitingForPowercycle,
                                },
                            ));
                        }

                        let dpu_ids = mh_snapshot.host_snapshot.associated_dpu_machine_ids();
                        Ok(StateHandlerOutcome::transition(
                            ManagedHostState::DpuDiscoveringState {
                                dpu_states: DpuDiscoveringStates {
                                    states: dpu_ids
                                        .iter()
                                        .map(|id| (*id, DpuDiscoveringState::Initializing))
                                        .collect(),
                                },
                            },
                        ))
                    }
                    ConfigureAstraState::WaitingForPowercycle => {
                        let basetime = mh_snapshot
                            .host_snapshot
                            .status
                            .last_reboot_requested
                            .as_ref()
                            .map(|x| x.time)
                            .unwrap_or(mh_snapshot.host_snapshot.state.version.timestamp());

                        if wait(&basetime, self.reachability_params.power_down_wait) {
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Waiting for host {} AC power cycle grace period",
                                mh_snapshot.host_snapshot.id
                            )));
                        }

                        let redfish_client = ctx
                            .services
                            .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                            .await?;
                        let power_state = host_power_state(redfish_client.as_ref()).await?;

                        // ACPowercycle can fall back to ForceOff when unsupported; ensure On.
                        if power_state != libredfish::PowerState::On
                            && power_state != libredfish::PowerState::PoweringOn
                        {
                            tracing::info!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                %power_state,
                                "Host not yet On after Astra AC power cycle; powering on"
                            );
                            handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::On)
                                .await?;
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Waiting for host {} to power on after Astra AC power cycle",
                                mh_snapshot.host_snapshot.id
                            )));
                        }

                        let dpu_ids = mh_snapshot.host_snapshot.associated_dpu_machine_ids();
                        Ok(StateHandlerOutcome::transition(
                            ManagedHostState::DpuDiscoveringState {
                                dpu_states: DpuDiscoveringStates {
                                    states: dpu_ids
                                        .iter()
                                        .map(|id| (*id, DpuDiscoveringState::Initializing))
                                        .collect(),
                                },
                            },
                        ))
                    }
                }
            }
            ManagedHostState::DpuDiscoveringState { .. } => {
                if mh_snapshot
                    .host_snapshot
                    .associated_dpu_machine_ids()
                    .is_empty()
                {
                    // GB200/300 dpu info not populated in expected machines and dpu not cabled up will go through here.
                    tracing::info!(
                        machine_id = %host_machine_id,
                        "Skipping to HostInit because machine has no DPUs"
                    );
                    Ok(StateHandlerOutcome::transition(
                        ManagedHostState::HostInit {
                            machine_state: MachineState::WaitingForPlatformConfiguration {
                                retry_count: 0,
                            },
                        },
                    ))
                } else {
                    // There used to be a `force_dpu_nic_mode` related return
                    // here that skipped DPU discovery entirely for operator-flagged
                    // NIC-mode hosts (site or individual), but we dropped it,
                    // becuse site-explorer doesn't have DPU IDs associated with
                    // the host in the first place; `associated_dpu_machine_ids`
                    // is empty, and the outer branch above already transitions
                    // to HostInit before we ever reach this. What's nice is, this
                    // also allows `Nic` hosts to get actively reconfigured
                    // to NIC mode via `set_nic_mode` during site-explorer ingestion,
                    // which is something we do, but `force_dpu_nic_mode` never did.
                    let mut state_handler_outcome = StateHandlerOutcome::do_nothing();
                    for dpu_snapshot in &mh_snapshot.dpu_snapshots {
                        state_handler_outcome = self
                            .dpu_handler
                            .handle_dpu_discovering_state(mh_snapshot, dpu_snapshot, ctx)
                            .await?;

                        if let outcome @ StateHandlerOutcome::Transition { .. } =
                            state_handler_outcome
                        {
                            return Ok(outcome);
                        }
                    }

                    Ok(state_handler_outcome)
                }
            }
            ManagedHostState::DPUInit { .. } => {
                self.dpu_handler
                    .handle_object_state(host_machine_id, mh_snapshot, &mh_state, ctx)
                    .await
            }

            ManagedHostState::HostInit { .. } => {
                self.host_handler
                    .handle_object_state(host_machine_id, mh_snapshot, &mh_state, ctx)
                    .await
            }

            ManagedHostState::Ready => {
                if let Some(outcome) = self
                    .handle_scout_heartbeat_timeout(mh_snapshot, ctx)
                    .await?
                {
                    return Ok(outcome);
                }

                if let Some(outcome) = maintenance::maintenance_transition_if_requested(mh_snapshot)
                {
                    return Ok(outcome);
                }

                // Health and maintenance handling take precedence. The request remains pending
                // until the host is safely Ready, then is cleared in the state-transition
                // transaction.
                if mh_snapshot.host_snapshot.decommission_requested {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db::machine::clear_decommission_requested(&mut txn, *host_machine_id).await?;
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::Decommissioning {
                            decommissioning_state: DecommissioningState::SuppressingSiteExplorer,
                        },
                    )
                    .with_txn(txn));
                }

                // An already-committed instance wins before disruptive boot
                // reconciliation. Allocation locks the machine row, while its
                // eligibility check rejects an earlier pending desired version.
                if mh_snapshot.instance.is_some() {
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::PreAssignedMeasuring {
                            spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                        },
                    ));
                }

                if let Some(next_state) =
                    pending_ready_boot_config_state(&mh_snapshot.host_snapshot)
                {
                    return Ok(StateHandlerOutcome::transition(next_state));
                }

                if let Some(outcome) = handle_bom_validation_requested(
                    &self.host_handler.host_handler_params,
                    mh_snapshot,
                    ctx.services,
                )
                .await?
                {
                    return Ok(outcome);
                }

                if host_reprovisioning_requested(mh_snapshot) {
                    if is_rack_level_reprovisioning(mh_snapshot) {
                        tracing::info!(
                            %host_machine_id,
                            "Rack-level firmware upgrade requested — entering HostReprovision"
                        );
                        return Ok(StateHandlerOutcome::transition(
                            ManagedHostState::HostReprovision {
                                retry_count: 1,
                                reprovision_state:
                                    HostReprovisionState::WaitingForRackFirmwareUpgrade,
                            },
                        ));
                    }

                    let outcome = self
                        .host_upgrade
                        .handle_host_reprovision(
                            mh_snapshot,
                            ctx,
                            host_machine_id,
                            HostFirmwareScenario::Ready,
                        )
                        .await?;
                    if matches!(outcome, StateHandlerOutcome::Transition { .. }) {
                        let health_report = create_host_update_health_report_hostfw();
                        let host_machine_id = *host_machine_id;

                        // The health report alert gets generated here, the machine update manager
                        // retains responsibilty for clearing it when we're done.
                        return Ok(outcome
                            .in_transaction(&ctx.services.db_pool, move |txn| {
                                async move {
                                    db::machine::insert_health_report(
                                        txn,
                                        &host_machine_id,
                                        health_report::HealthReportApplyMode::Merge,
                                        &health_report,
                                        false,
                                    )
                                    .await
                                }
                                .boxed()
                            })
                            .await??);
                    } else {
                        return Ok(outcome);
                    }
                }
                if let Some(outcome) =
                    handle_machine_validation_requested(ctx.services, mh_snapshot, false).await?
                {
                    return Ok(outcome);
                }

                // Check if DPU reprovisioning is requested
                if dpu_reprovisioning_needed(&mh_snapshot.dpu_snapshots) {
                    let mut dpus_for_reprov = vec![];
                    for dpu_snapshot in &mh_snapshot.dpu_snapshots {
                        if dpu_snapshot.reprovision_requested.is_some() {
                            handler_restart_dpu(
                                dpu_snapshot,
                                ctx,
                                mh_snapshot.host_snapshot.config.dpf.used_for_ingestion,
                            )
                            .await?;
                            ctx.pending_db_writes.push(
                                MachineWriteOp::UpdateDpuReprovisionStartTime {
                                    machine_id: dpu_snapshot.id,
                                    time: Utc::now(),
                                },
                            );
                            dpus_for_reprov.push(dpu_snapshot);
                        }
                    }

                    set_managed_host_topology_update_needed(
                        ctx.pending_db_writes,
                        &mh_snapshot.host_snapshot,
                        &dpus_for_reprov,
                    );

                    let reprov_state = ReprovisionState::next_substate_based_on_bfb_support(
                        self.enable_secure_boot,
                        mh_snapshot,
                        ctx.services.site_config.dpf_enabled,
                    );

                    let next_state = reprov_state.next_state_with_all_dpus_updated(
                        &mh_state,
                        &mh_snapshot.dpu_snapshots,
                        dpus_for_reprov
                            .into_iter()
                            .map(Machine::dpu_machine_id)
                            .collect::<Result<Vec<_>, _>>()?,
                    )?;

                    let health_override = create_host_update_health_report_dpufw();

                    // Mark the Host as in update.
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db::machine::insert_health_report(
                        &mut txn,
                        host_machine_id,
                        health_report::HealthReportApplyMode::Merge,
                        &health_override,
                        false,
                    )
                    .await?;
                    return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
                }

                // Check to see if measurement machine (i.e. attestation) state has changed
                // if so, just place it into the measuring state and let it be handled inside
                // the measurement state
                if self.host_handler.host_handler_params.attestation_enabled
                    && check_if_should_redo_measurements(
                        &mh_snapshot.host_snapshot.id,
                        &mut ctx.services.db_reader,
                    )
                    .await?
                {
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::Measuring {
                            measuring_state: MeasuringState::WaitingForMeasurements, // let's just start from the beginning
                        },
                    ));
                }

                // This feature has only been tested thoroughly on Dells and Lenovos
                if (mh_snapshot.host_snapshot.bmc_vendor().is_dell()
                    || mh_snapshot.host_snapshot.bmc_vendor().is_lenovo())
                    && mh_snapshot.host_snapshot.bios_password_set_time.is_none()
                {
                    tracing::info!(
                        "transitioning legacy {} host {} to UefiSetupState::UnlockHost while it is in ManagedHostState::Ready so that the BIOS password can be configured",
                        mh_snapshot.host_snapshot.bmc_vendor(),
                        mh_snapshot.host_snapshot.id
                    );
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::HostInit {
                            machine_state: MachineState::UefiSetup {
                                uefi_setup_info: UefiSetupInfo {
                                    uefi_password_jid: None,
                                    uefi_setup_state: UefiSetupState::UnlockHost,
                                },
                            },
                        },
                    ));
                }

                // Lowest precedence: only converge BMC credentials once the host
                // is otherwise idle in Ready, so rotation never contends with
                // instance creation, reprovision, validation, or measurements.
                // The site-flag gate and the operator force-converge override
                // live in `rotation::should_enter_bmc_rotation`.
                if rotation::should_enter_bmc_rotation(ctx.services, mh_snapshot).await? {
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::RotatingBmc { retry_count: 0 },
                    ));
                }

                // Same lowest-precedence idle-only rule as BMC rotation: converge
                // the host UEFI password only from an otherwise-idle Ready host.
                // The reboot the apply requires is acceptable for a pool host and
                // is gated by the site flag / force-converge override in
                // `host_uefi_rotation::should_enter_host_uefi_rotation`.
                if host_uefi_rotation::should_enter_host_uefi_rotation(ctx.services, mh_snapshot)
                    .await?
                {
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::RotatingHostUefi {
                            uefi_setup_info: UefiSetupInfo {
                                uefi_password_jid: None,
                                uefi_setup_state: UefiSetupState::UnlockHost,
                            },
                        },
                    ));
                }

                // Same lowest-precedence idle-only rule again, for each DPU's
                // UEFI password. A DPU change stages a Bios/Settings write and
                // commits it with a DPU restart, so it gets its own state keyed
                // to one DPU; the guard selects the next lagging or
                // force-requested DPU and the next sweep re-selects any others.
                if let Some(dpu_machine_id) =
                    dpu_uefi_rotation::select_dpu_for_uefi_rotation(ctx.services, mh_snapshot)
                        .await?
                {
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::RotatingDpuUefi { dpu_machine_id },
                    ));
                }

                // Same lowest-precedence idle-only rule again, for the host's
                // NIC lockdown keys. A rekey unlocks and relocks each SVPC
                // card via the DPA state machine + scout, so it must never run
                // under active tenancy; the site flag / force-converge override
                // live in `nic_lockdown_rotation::should_enter_nic_lockdown_rotation`.
                if nic_lockdown_rotation::should_enter_nic_lockdown_rotation(
                    ctx.services,
                    mh_snapshot,
                )
                .await?
                {
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::RotatingNicLockdown,
                    ));
                }

                // Releasing a DPF maintenance hold restarts DPU services, so it
                // belongs with the idle-only work above rather than ahead of it.
                dpu_action_handler::handle_pending_dpu_actions(
                    self.dpu_handler.dpf_sdk.as_deref(),
                    ctx,
                    mh_snapshot,
                )
                .await?;

                // Periodic BMC observation is deliberately Ready's final work,
                // so it cannot preempt lifecycle or operator-requested actions.
                boot_interface_observation::observe_verified_boot_interface(ctx, mh_snapshot).await
            }

            ManagedHostState::Decommissioning {
                decommissioning_state,
            } => match decommissioning_state {
                DecommissioningState::SuppressingSiteExplorer => {
                    decommissioning::handle_suppressing_site_explorer(mh_snapshot, ctx).await
                }
                DecommissioningState::DeconfiguringHost {
                    deconfiguring_state,
                } => {
                    decommissioning::handle_deconfiguring_host(
                        deconfiguring_state,
                        mh_snapshot,
                        ctx,
                    )
                    .await
                }
                DecommissioningState::DeconfiguringDpus { dpu_states } => {
                    decommissioning::handle_deconfiguring_dpus(
                        dpu_states,
                        mh_snapshot,
                        ctx,
                        self.dpu_handler.dpf_sdk.as_deref(),
                    )
                    .await
                }
                DecommissioningState::SuppressingOobDhcp => {
                    decommissioning::handle_suppressing_oob_dhcp(mh_snapshot, ctx).await
                }
                DecommissioningState::PowerCyclingHost => {
                    decommissioning::handle_power_cycling_host(mh_snapshot, ctx).await
                }
                DecommissioningState::PoweringOnHost => {
                    decommissioning::handle_powering_on_host(
                        mh_snapshot,
                        ctx,
                        self.reachability_params.power_down_wait,
                    )
                    .await
                }
                DecommissioningState::WaitingForOobDhcpAcknowledgement => {
                    decommissioning::handle_waiting_for_oob_dhcp_acknowledgement(mh_snapshot, ctx)
                        .await
                }
                DecommissioningState::SuppressingBmcDhcp => {
                    decommissioning::handle_suppressing_bmc_dhcp(mh_snapshot, ctx).await
                }
                DecommissioningState::FactoryResettingBmcs { completed } => {
                    decommissioning::handle_factory_resetting_bmcs(completed, mh_snapshot, ctx)
                        .await
                }
                DecommissioningState::WaitingForBmcDhcpAcknowledgement => {
                    decommissioning::handle_waiting_for_bmc_dhcp_acknowledgement(mh_snapshot, ctx)
                        .await
                }
                DecommissioningState::DeletingManagedCredentials => {
                    decommissioning::handle_deleting_managed_credentials(mh_snapshot, ctx).await
                }
                DecommissioningState::Decommissioned => Ok(StateHandlerOutcome::do_nothing()),
            },

            ManagedHostState::BootConfiguring {
                desired_version,
                desired_boot_interface,
                post_lock_verification_retry_count,
                boot_config_state,
            } => {
                handle_ready_boot_config(
                    ctx,
                    mh_snapshot,
                    &self.host_handler.host_handler_params.reachability_params,
                    Versioned {
                        value: desired_boot_interface.clone(),
                        version: *desired_version,
                    },
                    *post_lock_verification_retry_count,
                    boot_config_state.clone(),
                )
                .await
            }

            ManagedHostState::RotatingBmc { retry_count } => {
                // Hold site-explorer off every BMC of this managed host and wait
                // until it has acknowledged, before changing any credential. This
                // closes the window in which a probe could hit new-hardware /
                // old-Vault and persist the sticky `AvoidLockout` latch.
                let bmc_macs: Vec<_> = rotation::managed_host_bmc_endpoints(mh_snapshot)
                    .map(|e| e.device_mac)
                    .collect();
                if matches!(
                    site_explorer_pause::gate_before_credential_change(
                        &ctx.services.db_pool,
                        &bmc_macs,
                        site_explorer_pause::ROTATION_SUPPRESSION_REASON,
                    )
                    .await?,
                    GateDecision::Wait
                ) {
                    return Ok(StateHandlerOutcome::wait(
                        "waiting for site-explorer to acknowledge the BMC rotation suppression"
                            .to_string(),
                    ));
                }

                // One tick converges every BMC that needs work: force-requested
                // devices (bypassing backoff) and, when site-wide rotation is
                // enabled, any lagging host or DPU BMC -- handled together.
                let tick = rotation::rotate_managed_host_bmcs(ctx.services, mh_snapshot).await;
                match advance(tick, *retry_count, host_machine_id) {
                    // A BMC changed its hardware password but the per-device
                    // secret persist has not yet succeeded: hold in RotatingBmc
                    // (site-explorer stays suppressed, and the `Ready`-only
                    // allocation gate keeps the host non-allocatable) and re-tick.
                    // Do NOT resume site-explorer and do NOT clear a force
                    // request -- the rotation has not finished. The engine leaves
                    // the device `needs_rotation`, so a later tick's
                    // change-then-verify recovery re-persists and converges; the
                    // hold's upper bound is the state's time-in-state SLA.
                    RotationStep::WaitForCredentialStoreReconcile => Ok(StateHandlerOutcome::wait(
                        "BMC hardware changed but the per-device secret persist has not yet \
                         succeeded; holding rotation (site-explorer suppressed, host \
                         non-allocatable) until the credential store reconciles"
                            .to_string(),
                    )),
                    step @ (RotationStep::Settled | RotationStep::GaveUp) => {
                        // Both terminal steps return to Ready. Only a settled tick
                        // clears a one-shot force request: the forced attempt
                        // genuinely fired, so a satisfied (or unresolvable) request
                        // must not re-enter (only the machines observed as forced
                        // are cleared; see `clear_forced_bmc_requests`). GaveUp
                        // exhausted the transient-retry budget without the forced
                        // attempt cleanly running, so we leave the flag set and let
                        // the entry guard re-attempt on a later sweep rather than
                        // silently drop the operator's request.
                        // Resume site-explorer atomically with the return to Ready,
                        // so its skip window ends exactly when the rotation does; a
                        // settled tick also clears the one-shot force flag in the same
                        // transaction.
                        let mut txn = ctx.services.db_pool.begin().await?;
                        if matches!(step, RotationStep::Settled) {
                            rotation::clear_forced_bmc_requests(&mut txn, mh_snapshot).await?;
                        }
                        site_explorer_pause::resume_after_credential_change(
                            &mut txn,
                            &bmc_macs,
                            site_explorer_pause::ROTATION_SUPPRESSION_REASON,
                        )
                        .await?;
                        Ok(StateHandlerOutcome::transition(ManagedHostState::Ready).with_txn(txn))
                    }
                    RotationStep::Retry { retry_count } => Ok(StateHandlerOutcome::transition(
                        ManagedHostState::RotatingBmc { retry_count },
                    )),
                }
            }

            ManagedHostState::RotatingHostUefi { uefi_setup_info } => {
                host_uefi_rotation::handle_rotating_host_uefi(ctx, mh_snapshot, uefi_setup_info)
                    .await
            }

            ManagedHostState::RotatingDpuUefi { dpu_machine_id } => {
                dpu_uefi_rotation::handle_rotating_dpu_uefi(
                    ctx,
                    mh_snapshot,
                    (*dpu_machine_id).into(),
                )
                .await
            }

            ManagedHostState::RotatingNicLockdown => {
                nic_lockdown_rotation::handle_rotating_nic_lockdown(ctx, mh_snapshot).await
            }

            ManagedHostState::Assigned { instance_state: _ } => {
                // Process changes needed for instance.
                self.instance_handler
                    .handle_object_state(host_machine_id, mh_snapshot, &mh_state, ctx)
                    .await
            }

            ManagedHostState::WaitingForCleanup {
                cleanup_state,
                cleanup_context,
            } => {
                let redfish_client = ctx
                    .services
                    .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                    .await?;

                match cleanup_state {
                    CleanupState::Init => {
                        if mh_snapshot.host_snapshot.bmc_vendor().is_dell()
                            && let Some(boss_controller_id) = redfish_client
                                .get_boss_controller()
                                .await
                                .map_err(|e| redfish_error("get_boss_controller", e))?
                        {
                            let secure_erase_boss_state = match cleanup_context {
                                CleanupContext::Deprovision => SecureEraseBossState::UnlockHost,
                                CleanupContext::InitialDiscovery => {
                                    SecureEraseBossState::SecureEraseBoss
                                }
                            };
                            let next_state = waiting_for_cleanup_state(
                                CleanupState::SecureEraseBoss {
                                    secure_erase_boss_context: SecureEraseBossContext {
                                        boss_controller_id,
                                        secure_erase_jid: None,
                                        secure_erase_boss_state,
                                        iteration: Some(0),
                                    },
                                },
                                *cleanup_context,
                            );

                            return Ok(StateHandlerOutcome::transition(next_state));
                        }

                        let next_state = waiting_for_cleanup_state(
                            CleanupState::HostCleanup {
                                boss_controller_id: None,
                            },
                            *cleanup_context,
                        );

                        Ok(StateHandlerOutcome::transition(next_state))
                    }
                    CleanupState::SecureEraseBoss {
                        secure_erase_boss_context,
                    } => {
                        let boss_controller_id =
                            secure_erase_boss_context.boss_controller_id.clone();

                        match secure_erase_boss_context.secure_erase_boss_state {
                            SecureEraseBossState::UnlockHost => {
                                if matches!(cleanup_context, CleanupContext::Deprovision) {
                                    redfish_client
                                        .set_idrac_lockdown(EnabledDisabled::Disabled)
                                        .await
                                        .map_err(|e| redfish_error("set_idrac_lockdown", e))?;
                                }

                                let next_state = waiting_for_cleanup_state(
                                    CleanupState::SecureEraseBoss {
                                        secure_erase_boss_context: SecureEraseBossContext {
                                            boss_controller_id,
                                            secure_erase_jid: None,
                                            secure_erase_boss_state:
                                                SecureEraseBossState::SecureEraseBoss,
                                            iteration: secure_erase_boss_context.iteration,
                                        },
                                    },
                                    *cleanup_context,
                                );

                                Ok(StateHandlerOutcome::transition(next_state))
                            }
                            SecureEraseBossState::SecureEraseBoss => {
                                let jid = redfish_client
                                    .decommission_storage_controller(
                                        &secure_erase_boss_context.boss_controller_id,
                                    )
                                    .await
                                    .map_err(|e| {
                                        redfish_error("decommission_storage_controller", e)
                                    })?;

                                let next_state = waiting_for_cleanup_state(
                                    CleanupState::SecureEraseBoss {
                                        secure_erase_boss_context: SecureEraseBossContext {
                                            boss_controller_id,
                                            secure_erase_jid: jid,
                                            secure_erase_boss_state:
                                                SecureEraseBossState::WaitForJobCompletion,
                                            iteration: secure_erase_boss_context.iteration,
                                        },
                                    },
                                    *cleanup_context,
                                );

                                Ok(StateHandlerOutcome::transition(next_state))
                            }
                            SecureEraseBossState::WaitForJobCompletion => {
                                wait_for_boss_controller_job_to_complete(
                                    redfish_client.as_ref(),
                                    mh_snapshot,
                                )
                                .await
                            }
                            SecureEraseBossState::HandleJobFailure {
                                failure: _,
                                power_state: _,
                            } => {
                                handle_boss_job_failure(redfish_client.as_ref(), mh_snapshot, ctx)
                                    .await
                            }
                        }
                    }
                    CleanupState::HostCleanup { boss_controller_id } => {
                        if !cleanedup_after_state_transition(
                            mh_snapshot.host_snapshot.state.version,
                            mh_snapshot.host_snapshot.status.last_cleanup_time,
                        ) {
                            let status = trigger_reboot_if_needed(
                                &mh_snapshot.host_snapshot,
                                mh_snapshot,
                                None,
                                &self.reachability_params,
                                ctx,
                            )
                            .await?;
                            return Ok(StateHandlerOutcome::wait(status.status));
                        }

                        // Reboot host
                        handler_host_power_control(
                            mh_snapshot,
                            ctx,
                            SystemPowerControl::ForceRestart,
                        )
                        .await?;

                        let next_state = match boss_controller_id {
                            Some(boss_controller_id) => waiting_for_cleanup_state(
                                CleanupState::CreateBossVolume {
                                    create_boss_volume_context: CreateBossVolumeContext {
                                        boss_controller_id: boss_controller_id.to_string(),
                                        create_boss_volume_jid: None,
                                        create_boss_volume_state:
                                            CreateBossVolumeState::CreateBossVolume,
                                        iteration: Some(0),
                                    },
                                },
                                *cleanup_context,
                            ),
                            None => post_cleanup_state(*cleanup_context),
                        };

                        Ok(StateHandlerOutcome::transition(next_state))
                    }
                    CleanupState::CreateBossVolume {
                        create_boss_volume_context,
                    } => {
                        let boss_controller_id =
                            create_boss_volume_context.boss_controller_id.clone();
                        match create_boss_volume_context.create_boss_volume_state {
                            CreateBossVolumeState::CreateBossVolume => {
                                let jid = redfish_client
                                    .create_storage_volume(
                                        &create_boss_volume_context.boss_controller_id,
                                        "VD_0",
                                    )
                                    .await
                                    .map_err(|e| redfish_error("create_storage_volume", e))?;

                                let next_state = waiting_for_cleanup_state(
                                    CleanupState::CreateBossVolume {
                                        create_boss_volume_context: CreateBossVolumeContext {
                                            boss_controller_id,
                                            create_boss_volume_jid: jid,
                                            create_boss_volume_state:
                                                CreateBossVolumeState::WaitForJobScheduled,
                                            iteration: create_boss_volume_context.iteration,
                                        },
                                    },
                                    *cleanup_context,
                                );

                                Ok(StateHandlerOutcome::transition(next_state))
                            }
                            CreateBossVolumeState::WaitForJobScheduled => {
                                let job_id = create_boss_volume_context
                                    .create_boss_volume_jid
                                    .clone()
                                    .ok_or_else(|| {
                                        StateHandlerError::GenericError(eyre::eyre!(
                                            "could not find job ID in the create BOSS volume context"
                                        ))
                                    })?;

                                wait_for_boss_controller_job_to_scheduled(
                                    redfish_client.as_ref(),
                                    mh_snapshot,
                                    boss_controller_id,
                                    job_id,
                                    create_boss_volume_context.iteration,
                                )
                                .await
                            }
                            CreateBossVolumeState::RebootHost => {
                                redfish_client
                                    .power(SystemPowerControl::ForceRestart)
                                    .await
                                    .map_err(|e| redfish_error("ForceRestart", e))?;

                                let next_state = waiting_for_cleanup_state(
                                    CleanupState::CreateBossVolume {
                                        create_boss_volume_context: CreateBossVolumeContext {
                                            boss_controller_id,
                                            create_boss_volume_jid: create_boss_volume_context
                                                .create_boss_volume_jid
                                                .clone(),
                                            create_boss_volume_state:
                                                CreateBossVolumeState::WaitForJobCompletion,
                                            iteration: create_boss_volume_context.iteration,
                                        },
                                    },
                                    *cleanup_context,
                                );

                                Ok(StateHandlerOutcome::transition(next_state))
                            }
                            CreateBossVolumeState::WaitForJobCompletion => {
                                wait_for_boss_controller_job_to_complete(
                                    redfish_client.as_ref(),
                                    mh_snapshot,
                                )
                                .await
                            }
                            CreateBossVolumeState::LockHost => {
                                if matches!(cleanup_context, CleanupContext::Deprovision) {
                                    redfish_client
                                        .set_idrac_lockdown(EnabledDisabled::Enabled)
                                        .await
                                        .map_err(|e| redfish_error("set_idrac_lockdown", e))?;
                                }

                                let next_state = post_cleanup_state(*cleanup_context);

                                Ok(StateHandlerOutcome::transition(next_state))
                            }
                            CreateBossVolumeState::HandleJobFailure {
                                failure: _,
                                power_state: _,
                            } => {
                                handle_boss_job_failure(redfish_client.as_ref(), mh_snapshot, ctx)
                                    .await
                            }
                        }
                    }
                    CleanupState::DisableBIOSBMCLockdown => {
                        tracing::error!(
                            machine_id = %host_machine_id,
                            "DisableBIOSBMCLockdown state is not implemented. Machine stuck in unimplemented state.",
                        );
                        Err(StateHandlerError::InvalidHostState(
                            *host_machine_id,
                            Box::new(mh_state.clone()),
                        ))
                    }
                }
            }
            ManagedHostState::Created => {
                // Created is the insert-default state, and its exits are external to
                // this handler: machine-creation flows promote the row in the same
                // transaction that creates it, and a predicted host is promoted when
                // scout's discovery callout reports the real host. Transitioning here
                // would race those writers -- the handler's only job is to wait.
                tracing::debug!(
                    machine_id = %host_machine_id,
                    "Machine in Created; waiting for its creation flow or scout discovery",
                );
                Ok(StateHandlerOutcome::wait(
                    "Waiting for the machine creation flow or the scout discovery callout"
                        .to_string(),
                ))
            }
            ManagedHostState::ForceDeletion => {
                // Just ignore. Delete is done directly in api.rs::admin_force_delete_machine.
                tracing::info!(
                    machine_id = %host_machine_id,
                    "Machine is marked for forced deletion. Ignoring.",
                );
                Ok(StateHandlerOutcome::deleted())
            }
            ManagedHostState::Failed {
                details,
                machine_id,
                retry_count,
            } => {
                if let Some(outcome) = maintenance::maintenance_transition_if_requested(mh_snapshot)
                {
                    // Clear stale host failure details before accepting maintenance,
                    // matching other Failed recovery branches that exit with a txn.
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db::machine::clear_failure_details(host_machine_id, &mut txn).await?;
                    return Ok(outcome.with_txn(txn));
                }

                match details.cause {
                    // DPU discovery failed needs more logic to handle.
                    // DPU discovery can failed from multiple states init,
                    // waitingfornetworkinstall, reprov(waitingforfirmwareupgrade),
                    // reprov(waitingfornetworkinstall). Error handler must be aware of it and
                    // handle based on it.
                    // Another bigger problem is every discovery will need a
                    // fresh os install as scout is executed by cloud-init and it runs only
                    // once after os install. This has to be changed.
                    FailureCause::Discovery { .. } if machine_id.machine_type().is_host() => {
                        // If user manually reboots host, and discovery is successful then also it will come out
                        // of failed state.
                        if discovered_after_state_transition(
                            mh_snapshot.host_snapshot.state.version,
                            mh_snapshot.host_snapshot.status.last_discovery_time,
                        ) {
                            ctx.metrics
                                .machine_reboot_attempts_in_failed_during_discovery =
                                Some(*retry_count as u64);
                            // Anytime host discovery is successful, move to next state.
                            let mut txn = ctx.services.db_pool.begin().await?;
                            db::machine::clear_failure_details(machine_id, &mut txn).await?;
                            let next_state = ManagedHostState::HostInit {
                                machine_state: MachineState::WaitingForLockdown {
                                    lockdown_info: LockdownInfo {
                                        state: LockdownState::SetLockdown,
                                        mode: LockdownMode::Enable,
                                    },
                                },
                            };
                            return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
                        }

                        // Wait till failure_retry_time is over except first time.
                        // First time, host is already up and reported that discovery is failed.
                        // Let's reboot now immediately.
                        if *retry_count == 0 {
                            handler_host_power_control(
                                mh_snapshot,
                                ctx,
                                SystemPowerControl::ForceRestart,
                            )
                            .await?;
                            let next_state = ManagedHostState::Failed {
                                retry_count: retry_count + 1,
                                details: details.clone(),
                                machine_id: *machine_id,
                            };
                            return Ok(StateHandlerOutcome::transition(next_state));
                        }

                        if trigger_reboot_if_needed(
                            &mh_snapshot.host_snapshot,
                            mh_snapshot,
                            Some(*retry_count as i64),
                            &self.reachability_params,
                            ctx,
                        )
                        .await?
                        .increase_retry_count
                        {
                            let next_state = ManagedHostState::Failed {
                                retry_count: retry_count + 1,
                                details: details.clone(),
                                machine_id: *machine_id,
                            };
                            Ok(StateHandlerOutcome::transition(next_state))
                        } else {
                            Ok(StateHandlerOutcome::do_nothing())
                        }
                    }
                    FailureCause::NVMECleanFailed { .. } if machine_id.machine_type().is_host() => {
                        if cleanedup_after_state_transition(
                            mh_snapshot.host_snapshot.state.version,
                            mh_snapshot.host_snapshot.status.last_cleanup_time,
                        ) && mh_snapshot.host_snapshot.status.failure_details.failed_at
                            < mh_snapshot
                                .host_snapshot
                                .status
                                .last_cleanup_time
                                .unwrap_or_default()
                        {
                            // Cleaned up successfully after a failure.
                            let next_state = match &details.source {
                                FailureSource::StateMachineArea(StateMachineArea::HostInit) => {
                                    initial_discovery_waiting_state()
                                }
                                _ => waiting_for_cleanup_state(
                                    CleanupState::Init,
                                    CleanupContext::Deprovision,
                                ),
                            };
                            let mut txn = ctx.services.db_pool.begin().await?;
                            db::machine::clear_failure_details(machine_id, &mut txn).await?;
                            return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
                        }

                        if trigger_reboot_if_needed(
                            &mh_snapshot.host_snapshot,
                            mh_snapshot,
                            Some(*retry_count as i64),
                            &self.reachability_params,
                            ctx,
                        )
                        .await?
                        .increase_retry_count
                        {
                            let next_state = ManagedHostState::Failed {
                                retry_count: retry_count + 1,
                                details: details.clone(),
                                machine_id: *machine_id,
                            };
                            Ok(StateHandlerOutcome::transition(next_state))
                        } else {
                            Ok(StateHandlerOutcome::do_nothing())
                        }
                    }
                    FailureCause::MeasurementsRetired { .. }
                    | FailureCause::MeasurementsRevoked { .. }
                    | FailureCause::MeasurementsCAValidationFailed { .. } => {
                        if check_if_not_in_original_failure_cause_anymore(
                            &mh_snapshot.host_snapshot.id,
                            &mut ctx.services.db_reader,
                            &details.cause,
                            self.host_handler.host_handler_params.attestation_enabled,
                        )
                        .await?
                        {
                            // depending on the source of the failure, move it to the correct measuring state
                            match &details.source {
                                    FailureSource::StateMachineArea(area) => {
                                        match area{
                                            StateMachineArea::MainFlow => Ok(StateHandlerOutcome::transition(
                                                ManagedHostState::Measuring {
                                                    measuring_state: MeasuringState::WaitingForMeasurements
                                                })),
                                            StateMachineArea::HostInit => Ok(StateHandlerOutcome::transition(
                                                ManagedHostState::HostInit {
                                                    machine_state: MachineState::Measuring{
                                                        measuring_state: MeasuringState::WaitingForMeasurements
                                                    }
                                                })),
                                            StateMachineArea::AssignedInstance => Ok(StateHandlerOutcome::transition(
                                                ManagedHostState::PostAssignedMeasuring {
                                                    attestation_mode: AttestationMode::MeasuredBoot { measuring_state: MeasuringState::WaitingForMeasurements }
                                                }
                                            )),
                                            _ => Err(StateHandlerError::InvalidState(
                                                "Unimplemented StateMachineArea for FailureSource of  MeasurementsRetired, MeasurementsRevoked, MeasurementsCAValidationFailed"
                                                    .to_string(),
                                            ))
                                        }
                                    },
                                    _ => Err(StateHandlerError::InvalidState(
                                        "The source of MeasurementsRetired, MeasurementsRevoked, MeasurementsCAValidationFailed can only be StateMachine"
                                            .to_string(),
                                    ))
                                }
                        } else {
                            Ok(StateHandlerOutcome::do_nothing())
                        }
                    }
                    FailureCause::SpdmAttestationFailed { .. } => {
                        handle_spdm_attestation_failed_recovery(ctx, host_machine_id, details).await
                    }
                    FailureCause::MachineValidation { .. }
                        if machine_id.machine_type().is_host() =>
                    {
                        match handle_machine_validation_requested(ctx.services, mh_snapshot, true)
                            .await?
                        {
                            Some(outcome) => Ok(outcome),
                            None => Ok(StateHandlerOutcome::do_nothing()),
                        }
                    }
                    FailureCause::BiosSetupFailed { .. } if machine_id == host_machine_id => {
                        let recovered = ManagedHostState::HostInit {
                            machine_state: MachineState::SetBootOrder {
                                set_boot_order_info: Some(initial_set_boot_order_info()),
                            },
                        };
                        handle_bios_setup_failed_recovery(ctx, mh_snapshot, None, recovered).await
                    }
                    _ => {
                        // Do nothing.
                        // Handle error cause and decide how to recover if possible.
                        tracing::error!(
                            %machine_id,
                            "ManagedHost {} is in Failed state with machine/cause {}/{}. Failed at: {}, Ignoring.",
                            host_machine_id,
                            machine_id,
                            details.cause,
                            details.failed_at,
                        );
                        // TODO: Should this be StateHandlerError::ManualInterventionRequired ?
                        Ok(StateHandlerOutcome::do_nothing())
                    }
                }
            }
            ManagedHostState::DPUReprovision { .. } => {
                // Reaching host-level DPUReprovision with no DPUs is an
                // invariant violation -- the caller (Ready handler) only
                // enters this state when `dpu_reprovisioning_needed()` is
                // true, which requires non-empty DPUs. Without this guard
                // the empty loop below falls through to `do_nothing()` and
                // the host would sit in DPUReprovision forever.
                if !mh_snapshot.has_managed_dpus() {
                    return Err(StateHandlerError::GenericError(eyre!(
                        "DPUReprovision state entered on zero-DPU host {host_machine_id}; \
                         reprovision requires DPUs"
                    )));
                }

                let fw_config_snapshot = self.dpu_handler.hardware_models.create_snapshot();
                for dpu_snapshot in &mh_snapshot.dpu_snapshots {
                    // TODO: Optimization Possible: We can have another outcome something like
                    // TransitionNotPossible. This will be valid for the sync states (States where
                    // we wait for all DPUs to come in same state). If return value is
                    // TransitionNotPossible, means at least one DPU is not in ready to move into
                    // next state, thus no point of checking for next DPU. In this case, just break
                    // the loop.
                    if let outcome @ StateHandlerOutcome::Transition { .. } =
                        handle_dpu_reprovision(
                            mh_snapshot,
                            &self.reachability_params,
                            &MachineNextStateResolver,
                            dpu_snapshot,
                            ctx,
                            &fw_config_snapshot,
                            self.dpu_handler.dpf_sdk.as_deref(),
                        )
                        .await?
                    {
                        return Ok(outcome);
                    }
                }
                Ok(StateHandlerOutcome::do_nothing())
            }
            ManagedHostState::HostReprovision { .. } => {
                self.host_upgrade
                    .handle_host_reprovision(
                        mh_snapshot,
                        ctx,
                        host_machine_id,
                        HostFirmwareScenario::Ready,
                    )
                    .await
            }

            ManagedHostState::Maintenance { .. } => {
                maintenance::handle_maintenance(host_machine_id, mh_snapshot, ctx).await
            }

            // ManagedHostState::Measuring is introduced into the flow when
            // attestation_enabled is set to true (defaults to false), and
            // is triggered when a machine being in Ready state suddently
            // ceases being attested
            ManagedHostState::Measuring { measuring_state } => handle_measuring_state(
                measuring_state,
                &mh_snapshot.host_snapshot.id,
                &mut ctx.services.db_reader,
                self.host_handler.host_handler_params.attestation_enabled,
                mh_snapshot.host_snapshot.state.version.timestamp(),
                self.host_handler
                    .host_handler_params
                    .reachability_params
                    .waiting_for_measurements_timeout,
            )
            .await
            .map(|v| map_measuring_outcome_to_state_handler_outcome(&v, measuring_state))?,
            ManagedHostState::PostAssignedMeasuring { attestation_mode } => {
                match attestation_mode {
                    AttestationMode::MeasuredBoot { measuring_state } => {
                        if !self.host_handler.host_handler_params.attestation_enabled {
                            return Ok(StateHandlerOutcome::transition(
                                ManagedHostState::PostAssignedMeasuring {
                                    attestation_mode: AttestationMode::SpdmAttestation {
                                        spdm_measuring_state:
                                            SpdmMeasuringState::TriggerMeasurements,
                                    },
                                },
                            ));
                        }
                        handle_measuring_state(
                            measuring_state,
                            &mh_snapshot.host_snapshot.id,
                            &mut ctx.services.db_reader,
                            self.host_handler.host_handler_params.attestation_enabled,
                            mh_snapshot.host_snapshot.state.version.timestamp(),
                            self.host_handler
                                .host_handler_params
                                .reachability_params
                                .waiting_for_measurements_timeout,
                        )
                        .await
                        .map(|v| {
                            map_post_assigned_measuring_outcome_to_state_handler_outcome(
                                &v,
                                measuring_state,
                            )
                        })?
                    }
                    AttestationMode::SpdmAttestation {
                        spdm_measuring_state,
                    } => {
                        let next_skip_state = waiting_for_cleanup_state(
                            CleanupState::Init,
                            CleanupContext::Deprovision,
                        );
                        if !ctx.services.site_config.spdm_enabled {
                            return Ok(StateHandlerOutcome::transition(next_skip_state));
                        }
                        match spdm_measuring_state {
                            SpdmMeasuringState::TriggerMeasurements => {
                                handle_spdm_trigger_state(
                                    ctx.services,
                                    mh_snapshot,
                                    host_machine_id,
                                    ManagedHostState::PostAssignedMeasuring {
                                        attestation_mode: AttestationMode::SpdmAttestation {
                                            spdm_measuring_state: SpdmMeasuringState::PollResult,
                                        },
                                    },
                                    next_skip_state,
                                )
                                .await
                            }
                            SpdmMeasuringState::PollResult => {
                                handle_spdm_poll_state(
                                    &ctx.services.db_pool,
                                    host_machine_id,
                                    FailureSource::StateMachineArea(
                                        StateMachineArea::AssignedInstance,
                                    ),
                                    next_skip_state,
                                )
                                .await
                            }
                        }
                    }
                }
            }
            ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state,
            } => {
                let next_skip_state = ManagedHostState::StartAssignmentCycle;
                if !ctx.services.site_config.spdm_enabled {
                    return Ok(StateHandlerOutcome::transition(next_skip_state));
                }
                match spdm_measuring_state {
                    SpdmMeasuringState::TriggerMeasurements => {
                        handle_spdm_trigger_state(
                            ctx.services,
                            mh_snapshot,
                            host_machine_id,
                            ManagedHostState::PreAssignedMeasuring {
                                spdm_measuring_state: SpdmMeasuringState::PollResult,
                            },
                            next_skip_state,
                        )
                        .await
                    }
                    SpdmMeasuringState::PollResult => {
                        handle_spdm_poll_state(
                            &ctx.services.db_pool,
                            host_machine_id,
                            FailureSource::StateMachineArea(StateMachineArea::MainFlow),
                            next_skip_state,
                        )
                        .await
                    }
                }
            }
            ManagedHostState::StartAssignmentCycle => {
                // Instance is requested by user. Let's configure it.
                let mut txn = ctx.services.db_pool.begin().await?;

                // Clear if any reprovision (dpu or host) is set due to race scenario.
                Self::clear_host_update_alert_and_reprov(mh_snapshot, &mut txn).await?;

                let mut next_state = ManagedHostState::Assigned {
                    instance_state: InstanceState::DpaProvisioning,
                };

                if !ctx.services.site_config.ewethers_enabled {
                    // If DPA is not enabled, we don't need to do any DPA provisioning.
                    // So go directly to WaitingForDpaToBeReady state, where we will change
                    // the network status of our DPUs.
                    next_state = ManagedHostState::Assigned {
                        instance_state: InstanceState::WaitingForDpaToBeReady,
                    };
                }
                Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
            }
            ManagedHostState::BomValidating {
                bom_validating_state,
            } => {
                handle_bom_validation_state(
                    ctx,
                    &self.host_handler.host_handler_params,
                    mh_snapshot,
                    bom_validating_state,
                )
                .await
            }
            ManagedHostState::Validation { validation_state } => match validation_state {
                ValidationState::MachineValidation { machine_validation } => {
                    handle_machine_validation_state(
                        ctx,
                        machine_validation,
                        &self.host_handler.host_handler_params,
                        mh_snapshot,
                    )
                    .await
                }
            },
        }
    }

    /// Enables Astra on a single NIC.
    /// We pass in the expected interfaces and the NIC index to enable Astra on.
    /// Returns `true` when the NIC was enabled and the caller
    /// should AC-power-cycle the host for the change to take effect.
    async fn enable_astra_nic(
        &self,
        nic_index: u8,
        mh_snapshot: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        cx9_nics: &[&ExpectedInterface],
    ) -> Result<(), StateHandlerError> {
        tracing::info!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "Enabling Astra on NIC {nic_index}"
        );

        // Create the redfish client from the machine snapshot.
        let redfish_client = ctx
            .services
            .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
            .await?;

        // Get the MAC address of the NIC at the given index and see if it matches
        // the mac address in the passed in expected interface.
        let mac_address = redfish_client
            .get_spx_nic_mac_address(nic_index)
            .await
            .map_err(|e| redfish_error("get_spx_nic_mac_address", e))?
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: mh_snapshot.host_snapshot.id.to_string(),
                missing: "spx_nic_mac_address",
            })?;
        let mac_address = MacAddress::from_str(&mac_address).map_err(|e| {
            tracing::error!(
                machine_id = %mh_snapshot.host_snapshot.id,
                "Invalid SPX NIC MAC address {mac_address}: {e}"
            );
            StateHandlerError::GenericError(eyre!("invalid SPX NIC MAC address {mac_address}: {e}"))
        })?;

        // Find the expected CX9 NIC whose MAC matches what Redfish reported for
        // this card. If none matches, this card is not one we were told to
        // enable, so error out.
        let expected_nic = cx9_nics
            .iter()
            .copied()
            .find(|nic| nic.mac_address == mac_address)
            .ok_or_else(|| {
                tracing::error!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "No expected CX9 NIC matches Redfish MAC address {mac_address} for NIC {nic_index}"
                );
                StateHandlerError::GenericError(eyre!(
                    "no expected CX9 NIC matches MAC address {mac_address} for NIC {nic_index}"
                ))
            })?;

        // Now enable EastWestControlEnabled on this card.
        redfish_client
            .set_spx_nic_east_west_control_enabled(nic_index, true)
            .await
            .map_err(|e| redfish_error("set_spx_nic_east_west_control_enabled", e))?;

        // We have successfully enabled EastWestControlEnabled on this card.
        // Now add an entry for this NIC in the dpa_interface table.
        // Note that this entry will be added with the predicted host id, and when we
        // change the predicted host machine id to an actual machine id, we will have
        // to fix up the dpa_interfaces table.

        // Get the model and name of the NIC also, and we will use that information to create
        // the dpa_interface object.
        let nic_model_and_name = redfish_client
            .get_spx_nic_model_and_name(nic_index)
            .await
            .map_err(|e| redfish_error("get_spx_nic_model_and_name", e))?
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: mh_snapshot.host_snapshot.id.to_string(),
                missing: "spx_nic_model_and_name",
            })?;

        // Note that we are creating this dpa_interface object with a machine_id which is a predicted machine id.
        // When we change the predicted machine id to an actual machine id, we will have to fix up the dpa_interfaces table.
        let mut txn = ctx.services.db_pool.begin().await?;
        let mut dpa_interface = db::dpa_interface::ensure(
            NewDpaInterface {
                machine_id: mh_snapshot.host_snapshot.host_machine_id()?,
                mac_address,
                device_type: "Network Adapter Ethernet Interface".to_string(),
                pci_name: nic_model_and_name.name,
                device_description: Some(nic_model_and_name.model),
                interface_type: DpaInterfaceType::Astra,
            },
            &mut txn,
        )
        .await?;

        dpa_interface.underlay_ip = expected_nic.fixed_ip;

        // Call the update_ip routine to update the underlay_ip address of this dpa object,
        // obtaining the underlay ip from the fixed_ip field of the ExpectedInterface object.
        db::dpa_interface::update_ip(dpa_interface, true, &mut txn).await?;

        txn.commit().await?;

        tracing::info!(
            machine_id = %mh_snapshot.host_snapshot.id,
            mac_address = %mac_address,
            "Enabled Astra on NIC {nic_index} with MAC address {mac_address}"
        );

        Ok(())
    }

    /// Enables EastWestControl on every declared CX9 NIC.
    ///
    /// Returns `true` when at least one CX9 NIC was enabled and the caller
    /// should AC-power-cycle the host for the change to take effect.
    async fn enable_astra_all_nics(
        &self,
        mh_snapshot: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<bool, StateHandlerError> {
        // Skip Astra enablement entirely when the site has not opted in.
        if !ctx.services.site_config.astra_enabled || !ctx.services.site_config.ewethers_enabled {
            tracing::debug!(
                machine_id = %mh_snapshot.host_snapshot.id,
                "Astra not enabled for this site, skipping NIC enablement"
            );
            return Ok(false);
        }

        // Enable Astra if necessary
        // Look at the entry in the expected_machines table for this managed host, and retrieve the host_nics
        // field. If the host_nics is empty, just return.

        // its unlikely we got here without a bmc mac
        let Some(bmc_mac_address) = mh_snapshot.host_snapshot.status.bmc_info.mac else {
            tracing::debug!(
                machine_id = %mh_snapshot.host_snapshot.id,
                "No BMC MAC address configured"
            );
            return Err(StateHandlerError::MissingData {
                object_id: mh_snapshot.host_snapshot.id.to_string(),
                missing: "bmc_mac_address",
            });
        };

        let mut txn = ctx.services.db_pool.begin().await?;

        // Retrieve the expected_machines table entry for this managed host.
        let expected_machine =
            db::expected_machine::find_by_bmc_mac_address(txn.as_mut(), bmc_mac_address)
                .await
                .map_err(|err| {
                    tracing::error!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        %bmc_mac_address,
                        error = %err,
                        "Failed to look up expected machine for Astra enablement"
                    );
                    StateHandlerError::DBError(Box::new(err))
                })?;

        txn.commit().await?;

        // No expected-machine entry means there are no declared host NICs to act on.
        let Some(expected_machine) = expected_machine else {
            tracing::info!(
                machine_id = %mh_snapshot.host_snapshot.id,
                "No expected-machine entry found for Astra enablement"
            );
            return Ok(false);
        };

        let host_nics = expected_machine.data.interfaces;
        if host_nics.is_empty() {
            tracing::info!(
                machine_id = %mh_snapshot.host_snapshot.id,
                "No host NICs found for Astra enablement"
            );
            return Ok(false);
        }

        // At this point, we need to use Redfish to get all the CX cards in the host.
        // The end point to explore is /redfish/v1/Chassis/CX_$i

        let cx9_nics: Vec<_> = host_nics
            .iter()
            .filter(|nic| nic.nic_type.as_deref() == Some("CX9"))
            .collect();
        let enabled_any_cx9 = !cx9_nics.is_empty();

        for nic_index in 0..cx9_nics.len() {
            if let Err(e) = self
                .enable_astra_nic(nic_index as u8, mh_snapshot, ctx, &cx9_nics)
                .await
            {
                tracing::error!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    nic_index,
                    error = %e,
                    "Failed to enable Astra on CX9 NIC"
                );
                return Err(e);
            }
        }

        tracing::info!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "Enabled Astra on CX9 NICs: {enabled_any_cx9}"
        );

        Ok(enabled_any_cx9)
    }

    async fn handle_scout_heartbeat_timeout(
        &self,
        mh_snapshot: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
        let host_machine_id = &mh_snapshot.host_snapshot.id;
        let Some(last_scout_contact) = mh_snapshot.host_snapshot.status.last_scout_contact_time
        else {
            return Ok(None);
        };

        let since_last_contact = Utc::now().signed_duration_since(last_scout_contact);
        let timeout_threshold = self.reachability_params.scout_reporting_timeout;
        let scout_timeout_alert_exists = mh_snapshot
            .host_snapshot
            .health_reports
            .merges
            .contains_key("scout");

        if since_last_contact >= timeout_threshold {
            ctx.metrics.host_with_scout_heartbeat_timeout = Some(host_machine_id.to_string());
        }

        if since_last_contact >= timeout_threshold && !scout_timeout_alert_exists {
            let message = format!("Last scout heartbeat over {timeout_threshold} ago");
            let host_health = &ctx.services.site_config.host_health;
            let health_report = HealthReport::heartbeat_timeout(
                "scout".to_string(),
                "scout".to_string(),
                message,
                host_health.prevent_allocations_on_scout_heartbeat_timeout,
                host_health.suppress_external_alerting_on_scout_heartbeat_timeout,
            );

            let mut txn = ctx.services.db_pool.begin().await?;
            db::machine::insert_health_report(
                &mut txn,
                host_machine_id,
                HealthReportApplyMode::Merge,
                &health_report,
                false,
            )
            .await?;
            tracing::warn!(
                host_machine_id = %host_machine_id,
                last_scout_contact = %last_scout_contact,
                timeout_threshold = %timeout_threshold,
                "Scout heartbeat timeout detected, adding health alert"
            );
            return Ok(Some(StateHandlerOutcome::do_nothing().with_txn(txn)));
        }

        if since_last_contact < timeout_threshold && scout_timeout_alert_exists {
            let mut txn = ctx.services.db_pool.begin().await?;
            Self::clear_scout_timeout_alert(&mut txn, host_machine_id).await?;
            tracing::info!(
                host_machine_id = %host_machine_id,
                last_scout_contact = %last_scout_contact,
                timeout_threshold = %timeout_threshold,
                "Scout heartbeat recovered, removing health alert"
            );
            return Ok(Some(StateHandlerOutcome::do_nothing().with_txn(txn)));
        }

        Ok(None)
    }

    async fn handle_restart_dpu_reprovision_assigned_state(
        &self,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        host_machine_id: &MachineId,
        dpus_for_reprov: &[&Machine],
    ) -> Result<Option<ManagedHostState>, StateHandlerError> {
        // User approval must have received, otherwise reprovision has not
        // started.
        if let Err(err) =
            handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart).await
        {
            tracing::error!(%host_machine_id, "Host reboot failed with error: {err}");
        }
        set_managed_host_topology_update_needed(
            ctx.pending_db_writes,
            &state.host_snapshot,
            dpus_for_reprov,
        );

        let reprov_state = ReprovisionState::next_substate_based_on_bfb_support(
            self.enable_secure_boot,
            state,
            ctx.services.site_config.dpf_enabled,
        );
        Ok(Some(
            reprov_state.next_state_with_all_dpus_updated(
                &state.managed_state,
                &state.dpu_snapshots,
                dpus_for_reprov
                    .iter()
                    .map(|machine| machine.dpu_machine_id())
                    .collect::<Result<Vec<_>, _>>()?,
            )?,
        ))
    }

    // If current BMC FW allows to install bfb via redfish - performs redfish install,
    // otherwise reboots a DPU for iPXE install.
    async fn start_dpu_reprovision(
        &self,
        managed_state: &ManagedHostState,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        host_machine_id: &MachineId,
    ) -> Result<Option<ManagedHostState>, StateHandlerError> {
        let next_state: Option<ManagedHostState>;

        let dpus_for_reprov = state
            .dpu_snapshots
            .iter()
            .filter(|x| x.reprovision_requested.is_some())
            .collect_vec();

        match managed_state {
            ManagedHostState::Assigned {
                instance_state: InstanceState::DPUReprovision { .. } | InstanceState::Failed { .. },
            } => {
                // If we are here means already reprovision is going on, as validated by
                // can_restart_reprovision fucntion.
                next_state = self
                    .handle_restart_dpu_reprovision_assigned_state(
                        state,
                        ctx,
                        host_machine_id,
                        &dpus_for_reprov,
                    )
                    .await?;

                for dpu_id in dpus_for_reprov.iter().map(|d| d.id) {
                    ctx.pending_db_writes
                        .push(MachineWriteOp::ClearFailureDetails { machine_id: dpu_id });
                }
            }
            ManagedHostState::DPUReprovision { .. } => {
                set_managed_host_topology_update_needed(
                    ctx.pending_db_writes,
                    &state.host_snapshot,
                    &dpus_for_reprov,
                );

                next_state = Some(
                    ReprovisionState::next_substate_based_on_bfb_support(
                        self.enable_secure_boot,
                        state,
                        ctx.services.site_config.dpf_enabled,
                    )
                    .next_state_with_all_dpus_updated(
                        &state.managed_state,
                        &state.dpu_snapshots,
                        dpus_for_reprov
                            .iter()
                            .map(|m| m.dpu_machine_id())
                            .collect::<Result<Vec<_>, _>>()?,
                    )?,
                );
            }
            ManagedHostState::Failed { .. }
                if is_reprovision_restartable_failure(managed_state, host_machine_id) =>
            {
                set_managed_host_topology_update_needed(
                    ctx.pending_db_writes,
                    &state.host_snapshot,
                    &dpus_for_reprov,
                );

                // These failures leave the host in top-level Failed; restart must
                // reconstruct the reprovision map instead of trying to advance from Failed.
                next_state = Some(
                    ReprovisionState::next_substate_based_on_bfb_support(
                        self.enable_secure_boot,
                        state,
                        ctx.services.site_config.dpf_enabled,
                    )
                    .next_state_with_all_dpus_updated(
                        &ManagedHostState::Ready,
                        &state.dpu_snapshots,
                        dpus_for_reprov
                            .iter()
                            .map(|m| m.dpu_machine_id())
                            .collect::<Result<Vec<_>, _>>()?,
                    )?,
                );
            }
            _ => {
                next_state = None;
            }
        };

        if next_state.is_some() {
            // Restart all DPUs, sit back and relax.
            for dpu in dpus_for_reprov {
                ctx.pending_db_writes
                    .push(MachineWriteOp::UpdateDpuReprovisionStartTime {
                        machine_id: dpu.id,
                        time: Utc::now(),
                    });
                handler_restart_dpu(dpu, ctx, state.host_snapshot.config.dpf.used_for_ingestion)
                    .await?;
            }
            return Ok(next_state);
        }

        Ok(None)
    }
}

fn is_reprovision_restartable_failure(
    managed_state: &ManagedHostState,
    host_machine_id: &MachineId,
) -> bool {
    // BiosSetupFailed is always attributed to the host itself.
    matches!(
        managed_state,
        ManagedHostState::Failed {
            machine_id,
            details:
                FailureDetails {
                    cause: FailureCause::BiosSetupFailed { .. },
                    source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
                    ..
                },
            ..
        } if machine_id == host_machine_id
    ) ||
    // DpfProvisioning may be attributed to a specific DPU (e.g. DPU entered
    // error phase), so we do not guard on machine_id or source here.
    matches!(
        managed_state,
        ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::DpfProvisioning { .. },
                ..
            },
            ..
        }
    )
}

#[derive(Clone)]
struct FullFirmwareInfo<'a> {
    model: &'a str,
    to_install: &'a FirmwareEntry,
    component_type: &'a FirmwareComponentType,
    firmware_number: &'a u32,
}

/// need_host_fw_upgrade determines if the given endpoint needs a firmware upgrade based on the description in fw_info, and if so returns the FirmwareEntry matching the desired upgrade.
fn need_host_fw_upgrade(
    endpoint: &ExploredEndpoint,
    fw_info: &Firmware,
    firmware_type: FirmwareComponentType,
) -> Option<FirmwareEntry> {
    // Determining if we've disabled upgrades for this host is determined in machine_update_manager, not here; if it was disabled, nothing kicks it out of Ready.

    // First, find all current versions for this component. Some component types,
    // such as CX7, have several firmware inventory entries.
    let current_versions = endpoint.find_all_versions(fw_info, firmware_type);
    if current_versions.is_empty() {
        // Not listed, so we couldn't do an upgrade
        return None;
    }

    // Now find the desired version, if any matching inventory is not already on it.
    fw_info
        .components
        .get(&firmware_type)?
        .known_firmware
        .iter()
        .find(|firmware| {
            firmware.default
                && current_versions
                    .iter()
                    .any(|v| v.as_str() != firmware.version)
        })
        .cloned()
}

/// This function checks if reprovisioning is requested of a given DPU or not.
fn dpu_reprovisioning_needed(dpu_snapshots: &[Machine]) -> bool {
    dpu_snapshots
        .iter()
        .any(|x| x.reprovision_requested.is_some())
}

async fn handle_restart_verification(
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    const MAX_VERIFICATION_ATTEMPTS: i32 = 2;

    // Check host first
    if let Some(last_reboot) = &mh_snapshot.host_snapshot.status.last_reboot_requested
        && last_reboot.restart_verified == Some(false)
    {
        if mh_snapshot
            .host_snapshot
            .status
            .last_reboot_time
            .is_some_and(|completed| completed > last_reboot.time)
        {
            ctx.pending_db_writes
                .push(MachineWriteOp::UpdateRestartVerificationStatus {
                    machine_id: mh_snapshot.host_snapshot.id,
                    current_reboot: *last_reboot,
                    verified: Some(true),
                    attempts: 0,
                });
            return Ok(None);
        }

        let verification_attempts = last_reboot.verification_attempts.unwrap_or(0);

        let host_redfish_client = match ctx
            .services
            .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
            .await
        {
            Ok(client) => client,
            Err(err) => {
                tracing::debug!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    error = %err,
                    "Failed to create Redfish client for host during force-restart verification",
                );
                return Ok(None);
            }
        };

        let restart_found =
            match check_restart_in_logs(host_redfish_client.as_ref(), last_reboot.time).await {
                Ok(found) => found,
                Err(err) => {
                    tracing::debug!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        error = %err,
                        "Failed to fetch BMC logs for host during force-restart verification",
                    );
                    return Ok(None);
                }
            };

        if restart_found {
            ctx.pending_db_writes
                .push(MachineWriteOp::UpdateRestartVerificationStatus {
                    machine_id: mh_snapshot.host_snapshot.id,
                    current_reboot: *last_reboot,
                    verified: Some(true),
                    attempts: 0,
                });
            tracing::info!("Restart verified for host {}", mh_snapshot.host_snapshot.id);
            return Ok(None);
        }

        if verification_attempts >= MAX_VERIFICATION_ATTEMPTS {
            if matches!(
                &mh_snapshot.managed_state,
                ManagedHostState::Assigned {
                    instance_state: InstanceState::WaitingForRebootToReady,
                }
            ) {
                ctx.pending_db_writes
                    .push(MachineWriteOp::UpdateRestartVerificationStatus {
                        machine_id: mh_snapshot.host_snapshot.id,
                        current_reboot: *last_reboot,
                        verified: None,
                        attempts: 0,
                    });
                return Ok(Some(StateHandlerOutcome::wait(
                    "Waiting for host restart completion after BMC verification attempts were exhausted."
                        .to_string(),
                )));
            }

            host_redfish_client
                .power(SystemPowerControl::ForceRestart)
                .await
                .map_err(|e| redfish_error("restart host", e))?;

            ctx.pending_db_writes
                .push(MachineWriteOp::UpdateRestartVerificationStatus {
                    machine_id: mh_snapshot.host_snapshot.id,
                    current_reboot: *last_reboot,
                    verified: None,
                    attempts: 0,
                });

            tracing::info!(
                "Issued force-restart for host {} after {} failed verifications",
                mh_snapshot.host_snapshot.id,
                verification_attempts
            );
            return Ok(None);
        }

        ctx.pending_db_writes
            .push(MachineWriteOp::UpdateRestartVerificationStatus {
                machine_id: mh_snapshot.host_snapshot.id,
                current_reboot: *last_reboot,
                verified: Some(false),
                attempts: verification_attempts + 1,
            });

        return Ok(Some(StateHandlerOutcome::wait(format!(
            "Waiting for {} force-restart verification - attempt {}/{}",
            mh_snapshot.host_snapshot.id,
            verification_attempts + 1,
            MAX_VERIFICATION_ATTEMPTS
        ))));
    }

    // Check DPUs
    let mut pending_message = Vec::new();

    for dpu in &mh_snapshot.dpu_snapshots {
        if let Some(last_reboot) = dpu.status.last_reboot_requested
            && last_reboot.restart_verified == Some(false)
        {
            let verification_attempts = last_reboot.verification_attempts.unwrap_or(0);

            let dpu_redfish_client = match ctx
                .services
                .create_redfish_client_from_machine(dpu)
                .await
            {
                Ok(client) => client,
                Err(err) => {
                    tracing::debug!(
                        machine_id = %dpu.id,
                        error = %err,
                        "Failed to create Redfish client for DPU during force-restart verification",
                    );
                    ctx.pending_db_writes
                        .push(MachineWriteOp::UpdateRestartVerificationStatus {
                            machine_id: dpu.id,
                            current_reboot: last_reboot,
                            verified: None,
                            attempts: 0,
                        });
                    continue; // Skip verification, continue with state transition
                }
            };

            let restart_found =
                match check_restart_in_logs(dpu_redfish_client.as_ref(), last_reboot.time).await {
                    Ok(found) => found,
                    Err(err) => {
                        tracing::debug!(
                            machine_id = %dpu.id,
                            error = %err,
                            "Failed to fetch BMC logs for DPU during force-restart verification",
                        );

                        ctx.pending_db_writes.push(
                            MachineWriteOp::UpdateRestartVerificationStatus {
                                machine_id: dpu.id,
                                current_reboot: last_reboot,
                                verified: None,
                                attempts: 0,
                            },
                        );

                        continue; // Skip verification, continue with state transition
                    }
                };

            if restart_found {
                ctx.pending_db_writes
                    .push(MachineWriteOp::UpdateRestartVerificationStatus {
                        machine_id: dpu.id,
                        current_reboot: last_reboot,
                        verified: Some(true),
                        attempts: 0,
                    });
                tracing::info!("Restart verified for DPU {}", dpu.id);
            } else if verification_attempts >= MAX_VERIFICATION_ATTEMPTS {
                dpu_redfish_client
                    .power(SystemPowerControl::ForceRestart)
                    .await
                    .map_err(|e| redfish_error("reboot dpu", e))?;

                ctx.pending_db_writes
                    .push(MachineWriteOp::UpdateRestartVerificationStatus {
                        machine_id: dpu.id,
                        current_reboot: last_reboot,
                        verified: None,
                        attempts: 0,
                    });

                tracing::info!(
                    "Issued force-restart for DPU {} after {} failed verifications",
                    dpu.id,
                    verification_attempts
                );
            } else {
                ctx.pending_db_writes
                    .push(MachineWriteOp::UpdateRestartVerificationStatus {
                        machine_id: dpu.id,
                        current_reboot: last_reboot,
                        verified: Some(false),
                        attempts: verification_attempts + 1,
                    });

                pending_message.push(format!(
                    "DPU {} force-restart verification - attempt {}/{}",
                    dpu.id,
                    verification_attempts + 1,
                    MAX_VERIFICATION_ATTEMPTS
                ));
            }
        }
    }

    if !pending_message.is_empty() {
        Ok(Some(StateHandlerOutcome::wait(pending_message.join(", "))))
    } else {
        Ok(None)
    }
}

pub async fn check_restart_in_logs(
    redfish_client: &dyn Redfish,
    restart_time: DateTime<Utc>,
) -> Result<bool, RedfishError> {
    lazy_static::lazy_static! {
        // Vendor specific messages
        static ref SPECIFIC_RESET_KEYWORDS: HashSet<&'static str> = HashSet::from([
            "Server reset.",                                       // HPE
            "Server power restored.",                              // HPE
            "The server is restarted by chassis control command.", // Lenovo
            "DPU Warm Reset",                                      // Bluefield
            "BMC IP Address Deleted",                              // Bluefield
            "The property ResetType was assigned the value 'ForceWarmReboot' due to modification by the service.", // GB200
        ]);

        // Generic reset keywords
        static ref GENERIC_RESET_KEYWORDS: Vec<&'static str> =
            vec!["reset", "reboot", "restart", "power", "start"];
    }

    let logs = redfish_client.get_bmc_event_log(Some(restart_time)).await?;

    for log in &logs {
        tracing::debug!("BMC log message: {}", log.message);
    }

    let restart_found = logs.iter().any(|log| {
        // First check exact matches
        if SPECIFIC_RESET_KEYWORDS.contains(log.message.as_str()) {
            return true;
        }
        // Then generic keywords
        let lowercase_message = log.message.to_lowercase();
        GENERIC_RESET_KEYWORDS
            .iter()
            .any(|keyword| lowercase_message.contains(keyword))
    });

    Ok(restart_found)
}

// Function to wait for some time in state machine.
pub(super) fn wait(basetime: &DateTime<Utc>, wait_time: Duration) -> bool {
    let expected_time = *basetime + wait_time;
    let current_time = Utc::now();

    current_time < expected_time
}

fn is_dpu_up(state: &ManagedHostStateSnapshot, dpu_snapshot: &Machine) -> bool {
    let observation_time = dpu_snapshot
        .network_status_observation
        .as_ref()
        .map(|o| o.observed_at)
        .unwrap_or(DateTime::<Utc>::MIN_UTC);
    let state_change_time = state.host_snapshot.state.version.timestamp();

    if observation_time < state_change_time {
        return false;
    }

    true
}

fn is_dpu_observed_since(dpu_snapshot: &Machine, minimum_observed_at: DateTime<Utc>) -> bool {
    let observation_time = dpu_snapshot
        .network_status_observation
        .as_ref()
        .map(|o| o.observed_at)
        .unwrap_or(DateTime::<Utc>::MIN_UTC);

    observation_time >= minimum_observed_at
}

/// are_dpus_up_trigger_reboot_if_needed returns true if the dpu_agent indicates that the DPU has rebooted and is healthy.
/// otherwise returns false. triggers a reboot in case the DPU is down/bricked.
async fn are_dpus_up_trigger_reboot_if_needed(
    state: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> bool {
    for dpu_snapshot in &state.dpu_snapshots {
        if !is_dpu_up(state, dpu_snapshot) {
            match trigger_reboot_if_needed(dpu_snapshot, state, None, reachability_params, ctx)
                .await
            {
                Ok(_) => {}
                Err(e) => tracing::warn!("could not reboot dpu {}: {e}", dpu_snapshot.id),
            }
            return false;
        }
    }

    true
}

#[async_trait::async_trait]
impl StateHandler for MachineStateHandler {
    type State = ManagedHostStateSnapshot;
    type ControllerState = ManagedHostState;
    type ObjectId = HostMachineId;
    type ContextObjects = MachineStateHandlerContextObjects;

    // Note: extra_logfmt_logging_fields function to add additional
    // parameters that should be logged for each event inside span
    // crated by tracing instrumentation of handle_object_state.
    #[instrument(skip_all, fields(object_id=%host_machine_id, state=%_mh_state))]
    async fn handle_object_state(
        &self,
        host_machine_id: &HostMachineId,
        mh_snapshot: &mut ManagedHostStateSnapshot,
        _mh_state: &Self::ControllerState, // mh_snapshot above already contains it
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        // Refresh the per-object info/association series before any early
        // return: hosts stuck erroring must keep their join series alive, or
        // hold-period eviction drops exactly the machines being triaged.
        // (The aggregate metrics below deliberately stay behind the early
        // returns, preserving their long-standing exclusions.)
        if let Some(per_object_info) = &ctx.services.per_object_info {
            per_object_info.record(mh_snapshot);
        }

        if !mh_snapshot
            .host_snapshot
            .associated_dpu_machine_ids()
            .is_empty()
            && mh_snapshot.dpu_snapshots.is_empty()
        {
            let can_continue_without_dpu =
                ready_boot_config_handles_missing_dpu_snapshots(&mh_snapshot.managed_state);
            if !can_continue_without_dpu {
                tracing::error!(machine_id = %host_machine_id, "No DPU snapshot found for host");
                return Err(StateHandlerError::GenericError(eyre!(
                    "no DPU snapshot found"
                )));
            }

            // `ManagedHostState::BootConfiguring` performs the complete topology
            // DPU check. Later states return through `LockHost`, while `Prepare`
            // waits without touching the host until the snapshot is complete.
            tracing::warn!(
                machine_id = %host_machine_id,
                "Continuing ManagedHostState::BootConfiguring with missing DPU snapshots",
            );
        }

        self.record_metrics(mh_snapshot, ctx);
        self.record_health_history(mh_snapshot, ctx);

        // Handles power options based on the host's state and configuration settings.
        let PowerHandlingOutcome {
            power_options,
            continue_state_machine,
            msg,
        } = match mh_snapshot.host_snapshot.state.value {
            // A pending maintenance request takes precedence over power-manager
            // handling: `handle_power` can return `continue_state_machine = false`
            // (e.g. desired state Off), which would keep `attempt_state_transition`
            // -- and therefore `maintenance_transition_if_requested` -- from ever
            // running, starving the queued operation. Skipping power handling here
            // lets the request reach the Maintenance transition.
            ManagedHostState::Ready
                if self.power_options_config.enabled
                    && mh_snapshot
                        .host_snapshot
                        .machine_maintenance_requested
                        .is_none() =>
            {
                power::handle_power(mh_snapshot, ctx, &self.power_options_config).await?
            }
            _ => PowerHandlingOutcome::new(None, true, None),
        };

        // Clone the pool before we borrow ctx mutably
        let power_options_pool = ctx.services.db_pool.clone();

        let was_ready = matches!(mh_snapshot.managed_state, ManagedHostState::Ready);

        if !mh_snapshot.host_snapshot.config.dpf.used_for_ingestion {
            tracing::debug!(
                machine_id = %host_machine_id,
                docs = "https://docs.nvidia.com/infra-controller/documentation/getting-started/installation-options/dpf-setup",
                "iPXE provisioning strategy (internally) is deprecated; enable DPF management for DPUs to migrate"
            );
        }

        let mut result = if continue_state_machine {
            self.attempt_state_transition(host_machine_id, mh_snapshot, ctx)
                .await
        } else {
            Ok(StateHandlerOutcome::wait(format!(
                "State machine can't proceed due to power manager. {}",
                msg.unwrap_or_default()
            )))
        };

        if was_ready && let Ok(outcome) = result {
            if matches!(&outcome, StateHandlerOutcome::Transition { .. }) {
                let host_machine_id = *host_machine_id;
                result = Ok(outcome
                    .in_transaction(&ctx.services.db_pool, move |txn| {
                        async move {
                            Self::clear_scout_timeout_alert(txn, &host_machine_id).await?;
                            Ok::<(), StateHandlerError>(())
                        }
                        .boxed()
                    })
                    .await??);
            } else {
                result = Ok(outcome);
            }
        }

        // Persist power options before returning
        // They are persisted in an individual DB transaction in order to be unaffected
        // by the main state handling outcome
        if let Some(power_options) = power_options {
            let mut txn = power_options_pool.begin().await?;
            db::power_options::persist(&power_options, &mut txn).await?;
            txn.commit().await?;
        }

        result
    }
}

fn map_measuring_outcome_to_state_handler_outcome(
    measuring_outcome: &MeasuringOutcome,
    measuring_state: &MeasuringState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match measuring_outcome {
        MeasuringOutcome::NoChange => Ok(StateHandlerOutcome::wait(
            match measuring_state {
                MeasuringState::WaitingForMeasurements => {
                    "Waiting for machine to send measurement report"
                }
                MeasuringState::PendingBundle => {
                    "Waiting for matching measurement bundle for machine profile"
                }
            }
            .to_string(),
        )),
        MeasuringOutcome::WaitForGoldenValues => Ok(StateHandlerOutcome::transition(
            ManagedHostState::Measuring {
                measuring_state: MeasuringState::PendingBundle,
            },
        )),
        MeasuringOutcome::WaitForScoutToSendMeasurements => Ok(StateHandlerOutcome::transition(
            ManagedHostState::Measuring {
                measuring_state: MeasuringState::WaitingForMeasurements,
            },
        )),
        MeasuringOutcome::Unsuccessful((failure_details, machine_id)) => {
            Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                details: FailureDetails {
                    cause: failure_details.cause.clone(),
                    failed_at: failure_details.failed_at,
                    source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
                },
                machine_id: *machine_id,
                retry_count: 0,
            }))
        }
        MeasuringOutcome::PassedOk => Ok(StateHandlerOutcome::transition(ManagedHostState::Ready)),
    }
}

fn map_host_init_measuring_outcome_to_state_handler_outcome(
    measuring_outcome: &MeasuringOutcome,
    measuring_state: &MeasuringState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match measuring_outcome {
        MeasuringOutcome::NoChange => Ok(StateHandlerOutcome::wait(
            match measuring_state {
                MeasuringState::WaitingForMeasurements => {
                    "Waiting for machine to send measurement report"
                }
                MeasuringState::PendingBundle => {
                    "Waiting for matching measurement bundle for machine profile"
                }
            }
            .to_string(),
        )),
        MeasuringOutcome::WaitForGoldenValues => Ok(StateHandlerOutcome::transition(
            ManagedHostState::HostInit {
                machine_state: MachineState::Measuring {
                    measuring_state: MeasuringState::PendingBundle,
                },
            },
        )),
        MeasuringOutcome::WaitForScoutToSendMeasurements => Ok(StateHandlerOutcome::transition(
            ManagedHostState::HostInit {
                machine_state: MachineState::Measuring {
                    measuring_state: MeasuringState::WaitingForMeasurements,
                },
            },
        )),
        MeasuringOutcome::Unsuccessful((failure_details, machine_id)) => {
            Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                details: FailureDetails {
                    cause: failure_details.cause.clone(),
                    failed_at: failure_details.failed_at,
                    source: FailureSource::StateMachineArea(StateMachineArea::HostInit),
                },
                machine_id: *machine_id,
                retry_count: 0,
            }))
        }
        MeasuringOutcome::PassedOk => Ok(StateHandlerOutcome::transition(
            ManagedHostState::HostInit {
                machine_state: MachineState::SpdmMeasuring {
                    spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                },
            },
        )),
    }
}

async fn handle_bfb_install_state(
    state: &ManagedHostStateSnapshot,
    substate: InstallDpuOsState,
    dpu_snapshot: &Machine,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    next_state_resolver: &impl NextState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let dpu_machine_id = dpu_snapshot.dpu_machine_id()?;
    let dpu_machine_id = &dpu_machine_id;
    let dpu_redfish_client_result = ctx
        .services
        .create_redfish_client_from_machine(dpu_snapshot)
        .await;

    let dpu_redfish_client = match dpu_redfish_client_result {
        Ok(redfish_client) => redfish_client,
        Err(e) => {
            return Ok(StateHandlerOutcome::wait(format!(
                "Waiting for RedFish to become available: {:?}",
                e
            )));
        }
    };
    match substate {
        InstallDpuOsState::Completed => Ok(StateHandlerOutcome::transition(
            next_state_resolver.next_bfb_install_state(
                &state.managed_state,
                &InstallDpuOsState::Completed,
                dpu_machine_id,
            )?,
        )),
        InstallDpuOsState::InstallationError { .. } => Ok(StateHandlerOutcome::do_nothing()),

        InstallDpuOsState::InstallingBFB => {
            let task = dpu_redfish_client
                .update_firmware_simple_update(
                    "carbide-pxe.forge//public/blobs/internal/aarch64/forge.bfb",
                    vec!["redfish/v1/UpdateService/FirmwareInventory/DPU_OS".to_string()],
                    TransferProtocolType::HTTP,
                )
                .await
                .map_err(|e| redfish_error("update_firmware_simple_update", e))?;
            tracing::info!(
                "DPU {} OS install task {} submitted.",
                dpu_snapshot.id,
                task.id
            );
            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_bfb_install_state(
                    &state.managed_state,
                    &InstallDpuOsState::WaitForInstallComplete {
                        task_id: task.id,
                        progress: "0".to_string(),
                    },
                    dpu_machine_id,
                )?,
            ))
        }

        InstallDpuOsState::WaitForInstallComplete { task_id, .. } => {
            let task = dpu_redfish_client
                .get_task(task_id.as_str())
                .await
                .map_err(|e| redfish_error("get_task", e))?;

            tracing::info!(
                "DPU {} OS install task {}: {:#?}",
                dpu_snapshot.id,
                task.id,
                task.task_state
            );

            match task.task_state {
                Some(TaskState::Completed) => {
                    tracing::info!("Install BFB on {:#?} completed", dpu_snapshot.bmc_addr());
                    let next_state = next_state_resolver.next_bfb_install_state(
                        &state.managed_state,
                        &InstallDpuOsState::Completed,
                        dpu_machine_id,
                    )?;
                    Ok(StateHandlerOutcome::transition(next_state))
                }
                Some(TaskState::Exception) => {
                    let msg = format!(
                        "BFB install task {} on {:#?} failed: {}.",
                        task_id,
                        dpu_snapshot.bmc_addr(),
                        task.messages.iter().map(|t| t.message.clone()).join("\n")
                    );
                    tracing::error!(msg);
                    let next_state = next_state_resolver.next_bfb_install_state(
                        &state.managed_state,
                        &InstallDpuOsState::InstallationError { msg },
                        dpu_machine_id,
                    )?;
                    Ok(StateHandlerOutcome::transition(next_state))
                }
                Some(TaskState::Running) | Some(TaskState::New) | Some(TaskState::Starting) => {
                    let percent_complete = task
                        .percent_complete
                        .map_or("0".to_string(), |p| p.to_string());
                    Ok(StateHandlerOutcome::wait(format!(
                        "Waiting for BFB install to complete: {}%",
                        percent_complete
                    )))
                }
                task_state => {
                    let msg = format!(
                        "BFB install task {} on {:#?} failed ({:#?}): {}",
                        task_id,
                        dpu_snapshot.bmc_addr(),
                        task_state,
                        task.messages.iter().map(|t| t.message.clone()).join("\n")
                    );
                    tracing::error!(msg);
                    let next_state = next_state_resolver.next_bfb_install_state(
                        &state.managed_state,
                        &InstallDpuOsState::InstallationError { msg },
                        dpu_machine_id,
                    )?;
                    Ok(StateHandlerOutcome::transition(next_state))
                }
            }
        }
    }
}

fn map_post_assigned_measuring_outcome_to_state_handler_outcome(
    measuring_outcome: &MeasuringOutcome,
    measuring_state: &MeasuringState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match measuring_outcome {
        MeasuringOutcome::NoChange => Ok(StateHandlerOutcome::wait(
            match measuring_state {
                MeasuringState::WaitingForMeasurements => {
                    "Waiting for machine to send measurement report"
                }
                MeasuringState::PendingBundle => {
                    "Waiting for matching measurement bundle for machine profile"
                }
            }
            .to_string(),
        )),
        MeasuringOutcome::WaitForGoldenValues => Ok(StateHandlerOutcome::transition(
            ManagedHostState::PostAssignedMeasuring {
                attestation_mode: AttestationMode::MeasuredBoot {
                    measuring_state: MeasuringState::PendingBundle,
                },
            },
        )),
        MeasuringOutcome::WaitForScoutToSendMeasurements => Ok(StateHandlerOutcome::transition(
            ManagedHostState::PostAssignedMeasuring {
                attestation_mode: AttestationMode::MeasuredBoot {
                    measuring_state: MeasuringState::WaitingForMeasurements,
                },
            },
        )),
        MeasuringOutcome::Unsuccessful((failure_details, machine_id)) => {
            Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                details: FailureDetails {
                    cause: failure_details.cause.clone(),
                    failed_at: failure_details.failed_at,
                    source: FailureSource::StateMachineArea(StateMachineArea::AssignedInstance),
                },
                machine_id: *machine_id,
                retry_count: 0,
            }))
        }
        MeasuringOutcome::PassedOk => Ok(StateHandlerOutcome::transition(
            ManagedHostState::PostAssignedMeasuring {
                attestation_mode: AttestationMode::SpdmAttestation {
                    spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                },
            },
        )),
    }
}

// this is called when we are in the Ready state and checking
// if everything is ok in general
async fn check_if_should_redo_measurements(
    machine_id: &MachineId,
    txn: &mut PgPoolReader,
) -> Result<bool, StateHandlerError> {
    let (machine_state, ek_cert_verification_status) =
        get_measuring_prerequisites(machine_id, txn).await?;

    if !ek_cert_verification_status.signing_ca_found {
        return Ok(true);
    }
    match machine_state {
        MeasurementMachineState::Measured => Ok(false),
        _ => Ok(true),
    }
}

async fn check_if_not_in_original_failure_cause_anymore(
    machine_id: &MachineId,
    txn: &mut PgPoolReader,
    original_failure_cause: &FailureCause,
    attestation_enabled: bool,
) -> Result<bool, StateHandlerError> {
    if !attestation_enabled {
        return Ok(true);
    }
    let (_, ek_cert_verification_status) = get_measuring_prerequisites(machine_id, txn).await?;

    // if the failure cause was ca validation and it no longer is, then we can try
    // transitioning to the Measuring state to see where that takes us further
    if enum_discr(original_failure_cause)
        == enum_discr(&FailureCause::MeasurementsCAValidationFailed {
            err: "Dummy error".to_string(),
        })
        && ek_cert_verification_status.signing_ca_found
    {
        return Ok(true);
    }

    let current_failure_cause = super::get_measurement_failure_cause(txn, machine_id).await;

    if let Ok(current_failure_cause) = current_failure_cause {
        match original_failure_cause {
            FailureCause::MeasurementsRetired { .. } => {
                // if current/latest failure cause is the same
                // do nothing
                if enum_discr(&current_failure_cause)
                    == enum_discr(&FailureCause::MeasurementsRetired {
                        err: "Dummy error".to_string(),
                    })
                {
                    Ok(false) // nothing has changed
                } else {
                    Ok(true) // the state has changed
                }
            }
            FailureCause::MeasurementsRevoked { .. } => {
                // if current/latest failure cause is the same
                // do nothing
                if enum_discr(&current_failure_cause)
                    == enum_discr(&FailureCause::MeasurementsRevoked {
                        err: "Dummy error".to_string(),
                    })
                {
                    Ok(false) // nothing has changed
                } else {
                    Ok(true) // the state has changed
                }
            }
            FailureCause::MeasurementsCAValidationFailed { .. } => {
                if ek_cert_verification_status.signing_ca_found {
                    Ok(true) // it has changed
                } else {
                    Ok(false) // nothing has changed
                }
            }
            _ => Ok(true), // it has definitely changed (although we shouldn't be here)
        }
    } else {
        Ok(true) // something has definitely changed
    }
}

/// Return `DpuModel` if the explored endpoint is a DPU
pub fn identify_dpu(dpu_snapshot: &Machine) -> DpuModel {
    let model = dpu_snapshot
        .status
        .hardware_info
        .as_ref()
        .and_then(|hi| {
            hi.dpu_info
                .as_ref()
                .map(|di| di.part_description.to_string())
        })
        .unwrap_or("".to_string());
    model.into()
}

fn update_reprovision_targets_to_reprovision_state(
    state: &ManagedHostStateSnapshot,
    reprovision_state: ReprovisionState,
) -> Result<ManagedHostState, StateHandlerError> {
    // Host repair steps are shared, but only DPUs with active requests own reprovision state.
    let reprovision_target_dpu_ids = state
        .dpu_snapshots
        .iter()
        .filter(|dpu| dpu.reprovision_requested.is_some())
        .map(Machine::dpu_machine_id)
        .collect::<Result<Vec<_>, _>>()?;

    reprovision_state.next_state_with_all_dpus_updated(
        &state.managed_state,
        &state.dpu_snapshots,
        reprovision_target_dpu_ids,
    )
}

/// Handle workflow of DPU reprovision
#[allow(clippy::too_many_arguments)]
async fn handle_dpu_reprovision(
    state: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    next_state_resolver: &impl NextState,
    dpu_snapshot: &Machine,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    hardware_models: &FirmwareConfigSnapshot,
    dpf_sdk: Option<&dyn DpfOperations>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let dpu_machine_id = dpu_snapshot.dpu_machine_id()?;
    let dpu_machine_id = &dpu_machine_id;
    let reprovision_state = state
        .managed_state
        .as_reprovision_state(dpu_machine_id)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: dpu_machine_id.to_string(),
            missing: "dpu_state",
        })?;

    match reprovision_state {
        ReprovisionState::DpfStates { substate } => {
            let dpf = dpf_sdk.ok_or_else(|| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "DPF reprovision state reached but DPF is not configured"
                ))
            })?;
            dpf::handle_dpf_state(
                state,
                dpu_snapshot,
                substate,
                ctx,
                dpf,
                reachability_params.power_down_wait,
            )
            .await
        }
        ReprovisionState::InstallDpuOs { substate } => {
            handle_bfb_install_state(
                state,
                substate.clone(),
                dpu_snapshot,
                ctx,
                next_state_resolver,
            )
            .await
        }
        ReprovisionState::BmcFirmwareUpgrade { .. } => Ok(StateHandlerOutcome::transition(
            next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
        )),
        ReprovisionState::FirmwareUpgrade => {
            // Firmware upgrade is going on. Lets wait for it to over.
            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
            ))
        }
        ReprovisionState::WaitingForNetworkInstall => {
            if let Some(dpu_id) =
                try_wait_for_dpu_discovery(state, reachability_params, ctx, true, dpu_machine_id)
                    .await?
            {
                // Return Wait.
                return Ok(StateHandlerOutcome::wait(format!(
                    "DPU discovery for {dpu_id} is still not completed."
                )));
            }

            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
            ))
        }
        ReprovisionState::PoweringOffHost => {
            let dpus_states_for_reprov = &state
                .dpu_snapshots
                .iter()
                .filter(|x| x.reprovision_requested.is_some())
                .map(|x| {
                    let id = x.dpu_machine_id()?;
                    Ok::<_, StateHandlerError>(state.managed_state.as_reprovision_state(&id))
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect_vec();

            if !all_equal(dpus_states_for_reprov)? {
                return Ok(StateHandlerOutcome::wait(
                    "Waiting for DPUs to come in PoweringOffHost state.".to_string(),
                ));
            }

            handler_host_power_control(state, ctx, SystemPowerControl::ForceOff).await?;
            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
            ))
        }
        ReprovisionState::PowerDown => {
            let basetime = state
                .host_snapshot
                .status
                .last_reboot_requested
                .as_ref()
                .map(|x| x.time)
                .unwrap_or(state.host_snapshot.state.version.timestamp());

            if wait(&basetime, reachability_params.power_down_wait) {
                return Ok(StateHandlerOutcome::do_nothing());
            }

            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;
            let power_state = host_power_state(redfish_client.as_ref()).await?;

            // Host is not powered-off yet. Try again.
            if power_state != libredfish::PowerState::Off {
                tracing::error!(
                    machine_id = %state.host_snapshot.id,
                    %power_state,
                    "Host is still not powered off; forcing power off again"
                );
                handler_host_power_control(state, ctx, SystemPowerControl::ForceOff).await?;

                return Ok(StateHandlerOutcome::wait(format!(
                    "Host {} is not still powered off. Trying again.",
                    state.host_snapshot.id
                )));
            }

            // Mark all re-provisioned DPUs for topology update.
            let dpus_snapshots_for_reprov = &state
                .dpu_snapshots
                .iter()
                .filter(|x| x.reprovision_requested.is_some())
                .collect_vec();

            set_managed_host_topology_update_needed(
                ctx.pending_db_writes,
                &state.host_snapshot,
                dpus_snapshots_for_reprov,
            );

            handler_host_power_control(state, ctx, SystemPowerControl::On).await?;
            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
            ))
        }
        ReprovisionState::BufferTime => Ok(StateHandlerOutcome::transition(
            next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
        )),
        ReprovisionState::VerifyFirmareVersions => {
            // No need to compare version if machine is reprovisioned by DPF.
            if !state.host_snapshot.config.dpf.used_for_ingestion
                && let Some(outcome) =
                    check_fw_component_version(ctx, dpu_snapshot, hardware_models).await?
            {
                return Ok(outcome);
            }

            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_state(
                    &state.managed_state,
                    dpu_machine_id,
                    &state.host_snapshot,
                )?,
            ))
        }
        ReprovisionState::WaitingForNetworkConfig => {
            // Host boot repair is host-scoped, so wait until every reprovisioning
            // DPU has reached the same post-network-config point before touching
            // host BIOS.
            let dpus_states_for_reprov = &state
                .dpu_snapshots
                .iter()
                .filter(|x| x.reprovision_requested.is_some())
                .map(|x| {
                    let id = x.dpu_machine_id()?;
                    Ok::<_, StateHandlerError>(state.managed_state.as_reprovision_state(&id))
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect_vec();

            if !all_equal(dpus_states_for_reprov)? {
                return Ok(StateHandlerOutcome::wait(
                    "Waiting for DPUs to come in WaitingForNetworkConfig state.".to_string(),
                ));
            }

            // Validate all DPUs before host boot repair; subsequent states may
            // reboot the host or BMC and should not run while any DPU is still
            // unhealthy or unsynced.
            for dsnapshot in &state.dpu_snapshots {
                if !is_dpu_up(state, dsnapshot) {
                    let msg = format!("Waiting for DPU {} to come up", dsnapshot.id);
                    tracing::warn!(machine_id = %dsnapshot.id, "Waiting for DPU to come up");

                    let mut reboot_status = None;
                    // Only the DPU handled by this invocation should trigger its
                    // own recovery reboot; other DPUs are observed for gating.
                    if dpu_snapshot.id == dsnapshot.id {
                        reboot_status = Some(
                            trigger_reboot_if_needed(
                                dsnapshot,
                                state,
                                None,
                                reachability_params,
                                ctx,
                            )
                            .await?,
                        );
                    }

                    return Ok(StateHandlerOutcome::wait(format!(
                        "{msg};\nreboot_status: {reboot_status:#?}"
                    )));
                }

                if !managed_host_network_config_version_synced_and_dpu_healthy(
                    dsnapshot,
                    state.host_snapshot.network_config.version,
                ) {
                    tracing::warn!(
                        machine_id = %dsnapshot.id,
                        "Waiting for network to be ready for DPU"
                    );

                    // The install path already requested a DPU reboot. If this
                    // specific DPU remains unsynced, let trigger_reboot_if_needed
                    // decide whether enough time has elapsed for another reboot.
                    let mut reboot_status = None;
                    // Only the DPU handled by this invocation should trigger its
                    // own recovery reboot; other DPUs are observed for gating.
                    if dpu_snapshot.id == dsnapshot.id {
                        reboot_status = Some(
                            trigger_reboot_if_needed(
                                dsnapshot,
                                state,
                                None,
                                reachability_params,
                                ctx,
                            )
                            .await?,
                        );
                    }
                    // TODO: Make is_network_ready give us more details as a string
                    return Ok(StateHandlerOutcome::wait(format!(
                        "Waiting for DPU {} to sync network config/become healthy;\nreboot status: {reboot_status:#?}",
                        dsnapshot.id
                    )));
                }
            }

            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
            ))
        }
        ReprovisionState::PrepareHostBootRepair => {
            // Ensure host boot repair does not write through a locked BMC.
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;

            let next_state = match redfish_client.lockdown_status().await {
                Err(RedfishError::NotSupported(_)) => {
                    tracing::info!(
                        machine_id = %state.host_snapshot.id,
                        "BMC vendor does not support checking lockdown status during DPU reprovision host boot repair"
                    );
                    ReprovisionState::CheckHostBootConfig
                }
                Err(e) => {
                    tracing::warn!(
                        machine_id = %state.host_snapshot.id,
                        error = %e,
                        "Failed to fetch lockdown status during DPU reprovision host boot repair"
                    );
                    return Ok(StateHandlerOutcome::wait(format!(
                        "Failed to fetch lockdown status: {e}"
                    )));
                }
                Ok(lockdown_status) if !lockdown_status.is_fully_disabled() => {
                    tracing::info!(
                        machine_id = %state.host_snapshot.id,
                        "Lockdown is enabled during DPU reprovision host boot repair; disabling before boot config checks"
                    );
                    ReprovisionState::UnlockHostForBootRepair {
                        unlock_host_state: UnlockHostState::DisableLockdown,
                    }
                }
                Ok(_) => ReprovisionState::CheckHostBootConfig,
            };

            Ok(StateHandlerOutcome::transition(
                update_reprovision_targets_to_reprovision_state(state, next_state)?,
            ))
        }
        ReprovisionState::UnlockHostForBootRepair { unlock_host_state } => {
            // Mirror assigned platform config's unlock choreography before checking boot state.
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;

            let next_state = match unlock_host_state {
                UnlockHostState::DisableLockdown => {
                    redfish_client
                        .lockdown_bmc(EnabledDisabled::Disabled)
                        .await
                        .map_err(|e| redfish_error("lockdown_bmc", e))?;

                    let vendor = state.host_snapshot.bmc_vendor();

                    if vendor.is_supermicro() {
                        tracing::info!(
                            machine_id = %state.host_snapshot.id,
                            %vendor,
                            "BMC lockdown disabled; rebooting host so Redfish reflects actual boot order"
                        );
                        ReprovisionState::UnlockHostForBootRepair {
                            unlock_host_state: UnlockHostState::RebootHost,
                        }
                    } else {
                        tracing::info!(
                            machine_id = %state.host_snapshot.id,
                            %vendor,
                            "BMC lockdown disabled; skipping post-unlock reboot (not required for this vendor)"
                        );
                        ReprovisionState::CheckHostBootConfig
                    }
                }
                UnlockHostState::RebootHost => {
                    host_power_control(
                        redfish_client.as_ref(),
                        &state.host_snapshot,
                        SystemPowerControl::ForceRestart,
                        ctx,
                    )
                    .await
                    .map_err(|e| {
                        StateHandlerError::GenericError(eyre!(
                            "failed to ForceRestart host after disabling BMC lockdown: {}",
                            e
                        ))
                    })?;

                    ReprovisionState::UnlockHostForBootRepair {
                        unlock_host_state: UnlockHostState::WaitForUefiBoot,
                    }
                }
                UnlockHostState::WaitForUefiBoot => {
                    let entered_at = state.host_snapshot.state.version.timestamp();
                    if wait(&entered_at, reachability_params.uefi_boot_wait) {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for UEFI boot to complete on {} after post-unlock reboot; \
                             wait duration: {}, will proceed after {}",
                            state.host_snapshot.id,
                            reachability_params.uefi_boot_wait,
                            entered_at + reachability_params.uefi_boot_wait,
                        )));
                    }

                    ReprovisionState::CheckHostBootConfigAfterHostReboot
                }
            };

            Ok(StateHandlerOutcome::transition(
                update_reprovision_targets_to_reprovision_state(state, next_state)?,
            ))
        }
        ReprovisionState::CheckHostBootConfig => {
            // WaitingForNetworkConfig already accepted the DPU observation. Do
            // not require a newer observation just because the host state
            // version advanced while entering host boot repair.
            handle_dpu_reprovision_host_boot_config_check(
                ctx,
                state,
                reachability_params,
                HostBootConfigDpuFreshness::AlreadyValidated,
            )
            .await
        }
        ReprovisionState::CheckHostBootConfigAfterHostReboot => {
            // This path rebooted the host after unlocking, so require a DPU
            // observation newer than that reboot before trusting boot checks.
            handle_dpu_reprovision_host_boot_config_check(
                ctx,
                state,
                reachability_params,
                HostBootConfigDpuFreshness::SinceLastHostRebootRequest,
            )
            .await
        }
        ReprovisionState::ConfigureHostBoot { retry_count } => {
            handle_dpu_reprovision_host_boot_config_stage(
                ctx,
                state,
                reachability_params,
                HostBootConfigStage::ConfigureBios {
                    retry_count: *retry_count,
                },
            )
            .await
        }
        ReprovisionState::WaitingForHostBiosJob { bios_config_info } => {
            handle_dpu_reprovision_host_boot_config_stage(
                ctx,
                state,
                reachability_params,
                HostBootConfigStage::WaitingForBiosJob {
                    bios_config_info: bios_config_info.clone(),
                },
            )
            .await
        }
        ReprovisionState::PollingHostBiosSetup { retry_count } => {
            handle_dpu_reprovision_host_boot_config_stage(
                ctx,
                state,
                reachability_params,
                HostBootConfigStage::PollingBiosSetup {
                    retry_count: *retry_count,
                },
            )
            .await
        }
        ReprovisionState::SetHostBootOrder {
            set_boot_order_info,
        } => {
            handle_dpu_reprovision_host_boot_config_stage(
                ctx,
                state,
                reachability_params,
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info: set_boot_order_info.clone(),
                },
            )
            .await
        }
        ReprovisionState::LockHostAfterBootRepair => {
            // Preserve expected-machine lockdown policy after temporarily
            // opening the BMC for host boot repair.
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;

            if state.host_snapshot.host_profile.disable_lockdown {
                tracing::info!(
                    machine_id = %state.host_snapshot.id,
                    "Skipping lockdown re-enable in DPU reprovision per expected-machine config"
                );
            } else {
                match redfish_client.lockdown_bmc(EnabledDisabled::Enabled).await {
                    Ok(()) => {}
                    Err(RedfishError::NotSupported(_)) => {
                        tracing::info!(
                            machine_id = %state.host_snapshot.id,
                            "BMC vendor does not support re-enabling lockdown after DPU reprovision host boot repair"
                        );
                    }
                    Err(e) => return Err(redfish_error("lockdown_bmc", e)),
                }
            }

            Ok(StateHandlerOutcome::transition(
                update_reprovision_targets_to_reprovision_state(
                    state,
                    ReprovisionState::RebootHostBmc,
                )?,
            ))
        }
        ReprovisionState::RebootHostBmc => {
            // Work around for FORGE-3864
            // A NIC FW update from 24.39.2048 to 24.41.1000 can cause the Redfish service to become unavailable on Lenovos.
            // Forge initiates a NIC FW update in ReprovisionState::FirmwareUpgrade
            // At this point, all of the host's DPU have finished the NIC FW Update, been power cycled, and the ARM has come up on the DPU.
            if state.host_snapshot.bmc_vendor().is_lenovo() {
                tracing::info!(
                    machine_id = %state.host_snapshot.id,
                    "Initiating BMC reset of lenovo machine"
                );

                let redfish_client = ctx
                    .services
                    .create_redfish_client_from_machine(&state.host_snapshot)
                    .await?;

                if let Err(redfish_error) = redfish_client.bmc_reset(None).await {
                    tracing::warn!(
                        machine_id = %state.host_snapshot.id,
                        error = %redfish_error,
                        "Failed to reboot BMC through redfish, will try ipmitool"
                    );

                    let bmc_mac_address =
                        state.host_snapshot.status.bmc_info.mac.ok_or_else(|| {
                            StateHandlerError::MissingData {
                                object_id: state.host_snapshot.id.to_string(),
                                missing: "bmc_mac",
                            }
                        })?;

                    let bmc_ip_address =
                        state.host_snapshot.status.bmc_info.ip.ok_or_else(|| {
                            StateHandlerError::MissingData {
                                object_id: state.host_snapshot.id.to_string(),
                                missing: "bmc_ip",
                            }
                        })?;
                    let bmc_address = resolve_ipmi_address(bmc_ip_address, ctx).await?;

                    if let Err(ipmitool_error) = ctx
                        .services
                        .ipmi_tool
                        .bmc_cold_reset(
                            bmc_address,
                            &CredentialKey::BmcCredentials {
                                credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
                            },
                        )
                        .await
                    {
                        tracing::warn!(
                            machine_id = %state.host_snapshot.id,
                            error = %ipmitool_error,
                            "Failed to reset BMC through IPMI tool"
                        );

                        return Err(StateHandlerError::GenericError(eyre!(
                            "failed to reset BMC for {}; redfish error: {redfish_error}; ipmitool error: {ipmitool_error}",
                            &state.host_snapshot.id
                        )));
                    };
                }
            }

            Ok(StateHandlerOutcome::transition(
                next_state_resolver.next_state_with_all_dpus_updated(state, reprovision_state)?,
            ))
        }
        ReprovisionState::RebootHost => {
            // We can expect transient issues here in case we just rebooted the host's BMC and it has not come up yet
            handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart).await?;

            let mut txn = ctx.services.db_pool.begin().await?;

            // Clear reprovisioning requests only after the terminal host reboot is accepted.
            for dpu_snapshot in &state.dpu_snapshots {
                db::machine::clear_dpu_reprovisioning_request(&mut txn, &dpu_snapshot.id, false)
                    .await?;
            }

            // We need to wait for the host to reboot and submit its new Hardware information in
            // case of Ready.
            Ok(
                StateHandlerOutcome::transition(next_state_resolver.next_state(
                    &state.managed_state,
                    dpu_machine_id,
                    &state.host_snapshot,
                )?)
                .with_txn(txn),
            )
        }
        ReprovisionState::NotUnderReprovision => {
            if !dpf::deployment_migration_is_parked(state) {
                return Ok(StateHandlerOutcome::do_nothing());
            }
            // Deployment migration is host scoped, while this handler is
            // called once per DPU. Run it only for the first snapshot so one
            // controller iteration observes and changes the DPF graph once.
            if state.dpu_snapshots.first().map(|dpu| &dpu.id) != Some(dpu_machine_id) {
                return Ok(StateHandlerOutcome::do_nothing());
            }
            let dpf = dpf_sdk.ok_or_else(|| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "DPF deployment migration reached but DPF is not configured"
                ))
            })?;
            dpf::handle_dpf_deployment_migration(state, ctx, dpf).await
        }
    }
}

/// Checks host boot configuration for DPU reprovisioning and maps the shared
/// decision back into the reprovision state while preserving all active DPU
/// targets. The caller selects the observation-freshness requirement because
/// only the post-unlock path has rebooted the host.
async fn handle_dpu_reprovision_host_boot_config_check(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    dpu_freshness: HostBootConfigDpuFreshness,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&state.host_snapshot)
        .await?;

    let next_state = match check_host_boot_config(
        redfish_client.as_ref(),
        state,
        reachability_params,
        dpu_freshness,
        None,
        ctx,
    )
    .await?
    {
        HostBootConfigCheckOutcome::Wait(reason) => {
            return Ok(StateHandlerOutcome::wait(reason));
        }
        HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::ConfigureBios) => {
            ReprovisionState::ConfigureHostBoot { retry_count: 0 }
        }
        HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::SetBootOrder) => {
            ReprovisionState::SetHostBootOrder {
                set_boot_order_info: initial_set_boot_order_info(),
            }
        }
        HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::Complete) => {
            ReprovisionState::LockHostAfterBootRepair
        }
    };

    Ok(StateHandlerOutcome::transition(
        update_reprovision_targets_to_reprovision_state(state, next_state)?,
    ))
}

/// Handles a host boot-configuration stage owned by DPU reprovisioning.
///
/// Called by `handle_dpu_reprovision` for its host BIOS configuration,
/// vendor-job, polling, and boot-order substates. This adapter preserves the
/// active DPU reprovision targets, continues to `LockHostAfterBootRepair` on
/// completion, and attributes terminal failure to the lifecycle that owns the
/// reprovision.
async fn handle_dpu_reprovision_host_boot_config_stage(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    stage: HostBootConfigStage,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&state.host_snapshot)
        .await?;

    match run_host_boot_config_stage(
        ctx,
        reachability_params,
        redfish_client.as_ref(),
        state,
        None,
        stage,
    )
    .await?
    {
        HostBootConfigOutcome::Continue(stage) => {
            let reprovision_state = match stage {
                HostBootConfigStage::ConfigureBios { retry_count } => {
                    ReprovisionState::ConfigureHostBoot { retry_count }
                }
                HostBootConfigStage::WaitingForBiosJob { bios_config_info } => {
                    ReprovisionState::WaitingForHostBiosJob { bios_config_info }
                }
                HostBootConfigStage::PollingBiosSetup { retry_count } => {
                    ReprovisionState::PollingHostBiosSetup { retry_count }
                }
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info,
                } => ReprovisionState::SetHostBootOrder {
                    set_boot_order_info,
                },
            };

            Ok(StateHandlerOutcome::transition(
                update_reprovision_targets_to_reprovision_state(state, reprovision_state)?,
            ))
        }
        HostBootConfigOutcome::Complete => Ok(StateHandlerOutcome::transition(
            update_reprovision_targets_to_reprovision_state(
                state,
                ReprovisionState::LockHostAfterBootRepair,
            )?,
        )),
        HostBootConfigOutcome::Wait(reason) => Ok(StateHandlerOutcome::wait(reason)),
        HostBootConfigOutcome::Failed { failure } => Ok(StateHandlerOutcome::transition(
            dpu_reprovision_host_boot_failed_state(
                &state.managed_state,
                state.host_snapshot.id,
                failure,
            ),
        )),
    }
}

/// Build the correct failed state for host boot repair during DPU reprovision.
fn dpu_reprovision_host_boot_failed_state(
    current_state: &ManagedHostState,
    host_id: MachineId,
    failure: String,
) -> ManagedHostState {
    // Attribute the failure to the flow that owns the current reprovision.
    let source = FailureSource::StateMachineArea(
        if matches!(current_state, ManagedHostState::Assigned { .. }) {
            StateMachineArea::AssignedInstance
        } else {
            StateMachineArea::MainFlow
        },
    );

    // Reuse the existing BIOS setup failure category for machine_setup repair.
    let details = FailureDetails {
        cause: FailureCause::BiosSetupFailed { err: failure },
        failed_at: Utc::now(),
        source,
    };

    // Preserve the top-level assigned-state shape for tenant-owned hosts.
    if matches!(current_state, ManagedHostState::Assigned { .. }) {
        ManagedHostState::Assigned {
            instance_state: InstanceState::Failed {
                details,
                machine_id: host_id,
            },
        }
    } else {
        ManagedHostState::Failed {
            details,
            machine_id: host_id,
            retry_count: 0,
        }
    }
}

/// Load the host's predicted boot-interface candidates -- the interfaces a
/// zero-DPU or NIC-mode host offers before its first DHCP lease creates real
/// `machine_interfaces` rows. Empty once the host owns its rows (and for DPU
/// hosts, which get their primary row at attach), so the resolver simply finds
/// no prediction to fall back to.
async fn load_boot_predictions(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    machine_id: &MachineId,
) -> Result<Vec<PredictedMachineInterface>, StateHandlerError> {
    // A pooled read connection, not a transaction -- this read-only lookup runs
    // on the frequently-invoked boot-config path and needs no transaction.
    let mut conn = ctx.services.db_pool.acquire().await?;
    let predictions =
        db::predicted_machine_interface::find_by_machine_id(&mut conn, machine_id).await?;
    Ok(predictions)
}

/// Resolve one boot-config step from either its explicit target or current
/// interface state.
///
/// An explicit target is complete input, so resolving it does not require
/// reading mutable prediction rows.
async fn resolve_boot_interface_for_step(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    explicit_target: Option<&BootInterfaceTarget>,
) -> Result<BootInterfaceResolution, StateHandlerError> {
    if let Some(target) = explicit_target {
        return Ok(BootInterfaceResolution::Ready(target.clone()));
    }

    let predictions = load_boot_predictions(ctx, &mh_snapshot.host_snapshot.id).await?;
    Ok(resolve_boot_interface(mh_snapshot, &predictions))
}

// Returns true if update_manager flagged this managed host as needing its firmware examined
fn host_reprovisioning_requested(state: &ManagedHostStateSnapshot) -> bool {
    state.host_snapshot.host_reprovision_requested.is_some()
}

/// Returns true if the host reprovisioning request was initiated by a rack-level service
/// (i.e. the rack firmware upgrade flow).
fn is_rack_level_reprovisioning(state: &ManagedHostStateSnapshot) -> bool {
    state
        .host_snapshot
        .host_reprovision_requested
        .as_ref()
        .is_some_and(|req| req.initiator.starts_with("rack-"))
}

/// This function waits for DPU to finish discovery and reboots it.
pub async fn try_wait_for_dpu_discovery(
    state: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    is_reprovision_case: bool,
    current_dpu_machine_id: &MachineId,
) -> Result<Option<MachineId>, StateHandlerError> {
    // We are waiting for the `DiscoveryCompleted` RPC call to update the
    // `last_discovery_time` timestamp.
    // This indicates that all forge-scout actions have succeeded.
    for dpu_snapshot in &state.dpu_snapshots {
        if is_reprovision_case && dpu_snapshot.reprovision_requested.is_none() {
            // This is reprovision handling and this DPU is not under reprovisioning.
            continue;
        }
        if !discovered_after_state_transition(
            dpu_snapshot.state.version,
            dpu_snapshot.status.last_discovery_time,
        ) {
            // Reboot only the DPU for which the handler loop is called.
            if current_dpu_machine_id == &dpu_snapshot.id {
                let _status =
                    trigger_reboot_if_needed(dpu_snapshot, state, None, reachability_params, ctx)
                        .await?;
            }
            // TODO propagate the status.status message to a StateHandlerOutcome::Wait
            return Ok(Some(dpu_snapshot.id));
        }
    }

    Ok(None)
}

/// Returns Option<StateHandlerOutcome>:
///     If Some(_) means at least one fw component is not updated.
///     If None: All fw components are updated.
async fn check_fw_component_version(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpu_snapshot: &Machine,
    hardware_models: &FirmwareConfigSnapshot,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(dpu_snapshot)
        .await?;

    let redfish_component_name_map = HashMap::from([
        // Note: DPU uses different name for BMC Firmware as
        // BF2: 6d53cf4d_BMC_Firmware
        // BF3: BMC_Firmware
        (FirmwareComponentType::Nic, "DPU_NIC"),
        (FirmwareComponentType::Bmc, "BMC_Firmware"),
        (FirmwareComponentType::Uefi, "DPU_UEFI"),
        (FirmwareComponentType::Cec, "Bluefield_FW_ERoT"),
    ]);
    let inventories = redfish_client
        .get_software_inventories()
        .await
        .map_err(|e| redfish_error("get_software_inventories", e))?;

    for component in [
        FirmwareComponentType::Bmc,
        FirmwareComponentType::Cec,
        FirmwareComponentType::Nic,
    ] {
        let component_name = redfish_component_name_map.get(&component).unwrap();
        let inventory_id = inventories
            .iter()
            .find(|i| i.contains(component_name))
            .ok_or(StateHandlerError::FirmwareUpdateError(eyre!(
                "no inventory found that matches redfish component name: {component_name}; inventory list: {inventories:#?}",
            )))?;

        let inventory = match redfish_client.get_firmware(inventory_id).await {
            Ok(inventory) => inventory,
            Err(e) => {
                tracing::error!(
                    machine_id = %dpu_snapshot.id,
                    error = %e,
                    "redfish command get_firmware error"
                );
                return Err(redfish_error("get_firmware", e));
            }
        };

        if inventory.version.is_none() {
            let msg = format!("Unknown {component_name:?} version");
            tracing::error!(
                machine_id = %dpu_snapshot.id,
                component = ?component_name,
                "Unknown firmware version",
            );
            return Err(StateHandlerError::FirmwareUpdateError(eyre!(msg)));
        };

        let cur_version = inventory
            .version
            .unwrap_or("Unknown current installed BMC FW version".to_string());

        let model = identify_dpu(dpu_snapshot);

        let expected_version = hardware_models
            .find(bmc_vendor::BMCVendor::Nvidia, &model.to_string())
            .and_then(|fw| fw.components.get(&component).cloned())
            .and_then(|fw_component| {
                fw_component
                    .known_firmware
                    .iter()
                    .rfind(|fw_entry| !fw_entry.preingestion_exclusive_config)
                    .cloned()
            })
            .map(|f| f.version)
            .unwrap_or("Unknown current configured BMC FW version".to_string());

        if cur_version != expected_version {
            // CEC_MIN_RESET_VERSION="00.02.0180.0000"
            if component == FirmwareComponentType::Cec
                && version_compare::compare_to(&cur_version, "00.02.0180.0000", Cmp::Lt)
                    .is_ok_and(|x| x)
            {
                // For this case need to run host power cycle
                tracing::info!(
                    machine_id = %dpu_snapshot.id,
                    current_version = %cur_version,
                    expected_version = %expected_version,
                    "Need to launch host power cycle to update CEC FW"
                );
                return Ok(None);
            }

            tracing::debug!(
                machine_id = %dpu_snapshot.id,
                component = ?component,
                expected_version = %expected_version,
                current_version = %cur_version,
                "FW not yet at the expected version",
            );

            // Don't return Error. In case of the error, reboot time won't be updated in db.
            // This will cause continuous reboot of machine after first failure_retry_time is
            // passed.
            return Ok(Some(StateHandlerOutcome::wait(format!(
                "{:#?} FW not yet at the expected version. Expected: {}, Current: {}",
                component, expected_version, cur_version,
            ))));
        }

        tracing::info!(
            machine_id = %dpu_snapshot.id,
            component = ?component,
            expected_version = %expected_version,
            "FW updated successfully",
        );

        // BMC FW version need to update in machine_topology->bmc_info
        if component == FirmwareComponentType::Bmc
            && dpu_snapshot
                .status
                .bmc_info
                .clone()
                .firmware_version
                .is_some_and(|v| v != cur_version)
        {
            let bios_version: String = redfish_client
                .get_firmware("DPU_UEFI")
                .await
                .inspect_err(|e| {
                    tracing::error!(
                        machine_id = %dpu_snapshot.id,
                        error = %e,
                        "redfish command get_firmware error"
                    );
                })
                .ok()
                .and_then(|uefi| uefi.version)
                .unwrap_or_else(|| {
                    dpu_snapshot
                        .status
                        .hardware_info
                        .as_ref()
                        .and_then(|h| h.dmi_data.as_ref())
                        .map(|d| d.bios_version.clone())
                        .unwrap_or_default()
                });

            ctx.pending_db_writes.push(
                // This is safe to defer to pending_db_writes because the DPU snapshot already has
                // the machine ID needed for the topology update.
                MachineWriteOp::UpdateFirmwareVersionByMachineId {
                    machine_id: dpu_snapshot.id,
                    bmc_version: cur_version,
                    bios_version,
                },
            );
        }
    }

    // All good.
    Ok(None)
}

fn set_managed_host_topology_update_needed(
    pending_db_writes: &mut DbWriteBatch,
    host_snapshot: &Machine,
    dpus: &[&Machine],
) {
    //Update it for host and DPU both.
    for dpu_snapshot in dpus {
        pending_db_writes.push(MachineWriteOp::SetTopologyUpdateNeeded {
            machine_id: dpu_snapshot.id,
            value: true,
        });
    }

    pending_db_writes.push(MachineWriteOp::SetTopologyUpdateNeeded {
        machine_id: host_snapshot.id,
        value: true,
    });
}

/// This function returns failure cause for both host and dpu.
fn get_failed_state(state: &ManagedHostStateSnapshot) -> Option<(MachineId, FailureDetails)> {
    // Return updated state only for errors which should cause machine to move into failed
    // state.
    if state.host_snapshot.status.failure_details.cause != FailureCause::NoError {
        return Some((
            state.host_snapshot.id,
            state.host_snapshot.status.failure_details.clone(),
        ));
    } else {
        for dpu_snapshot in &state.dpu_snapshots {
            // In case of the DPU, use first failed DPU and recover it before moving forward.
            if dpu_snapshot.status.failure_details.cause != FailureCause::NoError {
                return Some((dpu_snapshot.id, dpu_snapshot.status.failure_details.clone()));
            }
        }
    }

    None
}

/// A `StateHandler` implementation for DPU machines
#[derive(Debug, Clone)]
pub struct DpuMachineStateHandler {
    dpu_nic_firmware_initial_update_enabled: bool,
    hardware_models: FirmwareConfig,
    reachability_params: ReachabilityParams,
    enable_secure_boot: bool,
    pub dpf_sdk: Option<Arc<dyn DpfOperations>>,
}

pub fn is_bf4_dmi_product(product: &str) -> bool {
    let product = product.to_uppercase();
    product.contains("B4") || product.contains("BLUEFIELD-4")
}

/// Whether DPF owns platform configuration for this BF4 DPU.
///
/// The persisted ingestion marker prevents an enabled-but-unused DPF
/// configuration from changing the legacy provisioning path.
pub fn is_dpf_managed_bf4(
    state: &ManagedHostStateSnapshot,
    dpu_snapshot: &Machine,
) -> Result<bool, StateHandlerError> {
    if !state.host_snapshot.config.dpf.used_for_ingestion {
        return Ok(false);
    }

    let product = dpu_snapshot
        .status
        .hardware_info
        .as_ref()
        .and_then(|x| x.dmi_data.as_ref())
        .map(|x| x.product_name.trim())
        .filter(|x| !x.is_empty())
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: dpu_snapshot.id.to_string(),
            missing: "product_name in topology",
        })?;

    Ok(is_bf4_dmi_product(product))
}

impl DpuMachineStateHandler {
    pub fn new(
        dpu_nic_firmware_initial_update_enabled: bool,
        hardware_models: FirmwareConfig,
        reachability_params: ReachabilityParams,
        enable_secure_boot: bool,
        dpf_sdk: Option<Arc<dyn DpfOperations>>,
    ) -> Self {
        DpuMachineStateHandler {
            dpu_nic_firmware_initial_update_enabled,
            hardware_models,
            reachability_params,
            enable_secure_boot,
            dpf_sdk,
        }
    }

    async fn is_secure_boot_disabled(
        &self,
        // passing in dpu_machine_id only for testing
        dpu_machine_id: &MachineId,
        dpu_redfish_client: &dyn Redfish,
    ) -> Result<bool, StateHandlerError> {
        let secure_boot_status = dpu_redfish_client
            .get_secure_boot()
            .await
            .map_err(|e| redfish_error("disable_secure_boot", e))?;

        let secure_boot_enable =
            secure_boot_status
                .secure_boot_enable
                .ok_or(StateHandlerError::MissingData {
                    object_id: dpu_machine_id.to_string(),
                    missing: "expected secure_boot_enable_field set in secure boot response",
                })?;

        let secure_boot_current_boot =
            secure_boot_status
                .secure_boot_current_boot
                .ok_or(StateHandlerError::MissingData {
                    object_id: dpu_machine_id.to_string(),
                    missing: "expected secure_boot_enable_field set in secure boot response",
                })?;

        Ok(!secure_boot_enable && !secure_boot_current_boot.is_enabled())
    }

    async fn handle_dpu_discovering_state(
        &self,
        state: &ManagedHostStateSnapshot,
        dpu_snapshot: &Machine,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let dpu_machine_id = dpu_snapshot.dpu_machine_id()?;
        let dpu_machine_id = &dpu_machine_id;
        let current_dpu_state = match &state.managed_state {
            ManagedHostState::DpuDiscoveringState { dpu_states } => dpu_states
                .states
                .get(dpu_machine_id)
                .ok_or_else(|| StateHandlerError::MissingData {
                    object_id: dpu_machine_id.to_string(),
                    missing: "dpu_state",
                })?,
            _ => {
                return Err(StateHandlerError::InvalidState(
                    "Unexpected state.".to_string(),
                ));
            }
        };

        let dpu_redfish_client_result = ctx
            .services
            .create_redfish_client_from_machine(dpu_snapshot)
            .await;

        let dpu_redfish_client = match dpu_redfish_client_result {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                return Ok(StateHandlerOutcome::wait(format!(
                    "Waiting for RedFish to become available: {:?}",
                    e
                )));
            }
        };

        match current_dpu_state {
            DpuDiscoveringState::Initializing => {
                let next_state = DpuDiscoveringState::Configuring
                    .next_state(&state.managed_state, dpu_machine_id)?;
                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuDiscoveringState::Configuring => {
                let next_state = DpuDiscoveringState::EnableRshim
                    .next_state(&state.managed_state, dpu_machine_id)?;
                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuDiscoveringState::EnableRshim => {
                dpu_redfish_client
                    .enable_rshim_bmc()
                    .await
                    .inspect_err(|e| {
                        tracing::info!(
                            dpu_machine_id = %dpu_machine_id,
                            error = %e,
                            "failed to enable rshim on DPU"
                        )
                    })
                    .ok();

                let next_dpu_discovering_state =
                    DpuDiscoveringState::next_substate_based_on_bfb_support(
                        self.enable_secure_boot,
                        state,
                        ctx.services.site_config.dpf_enabled,
                    );

                tracing::info!(
                    dpu_machine_id = %dpu_machine_id,
                    bmc_firmware_version = %dpu_snapshot
                        .status
                        .bmc_info
                        .firmware_version
                        .clone()
                        .unwrap_or("unknown".to_string()),
                    next_state = %next_dpu_discovering_state,
                    "DPU discovery state selected."
                );

                let next_state =
                    next_dpu_discovering_state.next_state(&state.managed_state, dpu_machine_id)?;
                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuDiscoveringState::EnableSecureBoot {
                count,
                enable_secure_boot_state,
                ..
            } => {
                self.set_secure_boot(
                    *count,
                    state,
                    enable_secure_boot_state.clone(),
                    true,
                    dpu_snapshot,
                    dpu_redfish_client.as_ref(),
                )
                .await
            }
            // The proceure to disable secure boot is documented on page 58-59 here: https://docs.nvidia.com/networking/display/nvidia-bluefield-management-and-initial-provisioning.pdf
            DpuDiscoveringState::DisableSecureBoot {
                disable_secure_boot_state,
                count,
            } => {
                self.set_secure_boot(
                    *count,
                    state,
                    disable_secure_boot_state
                        .clone()
                        .unwrap_or(SetSecureBootState::CheckSecureBootStatus),
                    false,
                    dpu_snapshot,
                    dpu_redfish_client.as_ref(),
                )
                .await
            }

            DpuDiscoveringState::SetUefiHttpBoot => {
                // This configures the DPU to boot once from UEFI HTTP.
                //
                // NOTE: since we don't have interface names yet (see comment about UEFI not
                // guaranteed to have POSTed), it will loop through all the interfaces between
                // IPv4, IPv6 so it may take a while.
                //
                dpu_redfish_client
                    .boot_once(Boot::UefiHttp)
                    .map_err(|e| redfish_error("boot_once", e))
                    .await?;

                let next_state = DpuDiscoveringState::RebootAllDPUS
                    .next_state(&state.managed_state, dpu_machine_id)?;
                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuDiscoveringState::RebootAllDPUS => {
                if !state.managed_state.all_dpu_states_in_sync()? {
                    return Ok(StateHandlerOutcome::wait(
                        "Waiting for all dpus to finish configuring.".to_string(),
                    ));
                }

                if dpf_based_dpu_provisioning_possible(state, self.dpf_sdk.is_some(), false) {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db::machine::mark_machine_ingestion_done_with_dpf(
                        &mut txn,
                        &state.host_snapshot.id,
                    )
                    .await?;

                    let next_state = DpuInitState::DpfStates {
                        state: model::machine::DpfState::Provisioning,
                    }
                    .next_state_with_all_dpus_updated(&state.managed_state)?;

                    return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
                }

                for dpu_snapshot in &state.dpu_snapshots {
                    handler_restart_dpu(
                        dpu_snapshot,
                        ctx,
                        state.host_snapshot.config.dpf.used_for_ingestion,
                    )
                    .await?;
                }
                let next_state =
                    DpuInitState::Init.next_state_with_all_dpus_updated(&state.managed_state)?;
                Ok(StateHandlerOutcome::transition(next_state))
            }
        }
    }

    async fn handle_dpuinit_state(
        &self,
        state: &ManagedHostStateSnapshot,
        dpu_snapshot: &Machine,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let dpu_machine_id = dpu_snapshot.dpu_machine_id()?;
        let dpu_machine_id = &dpu_machine_id;
        let dpu_state = match &state.managed_state {
            ManagedHostState::DPUInit { dpu_states } => dpu_states
                .states
                .get(dpu_machine_id)
                .ok_or_else(|| StateHandlerError::MissingData {
                    object_id: dpu_machine_id.to_string(),
                    missing: "dpu_state",
                })?,
            _ => {
                return Err(StateHandlerError::InvalidState(
                    "Unexpected state.".to_string(),
                ));
            }
        };
        match &dpu_state {
            DpuInitState::InstallDpuOs { substate } => {
                handle_bfb_install_state(
                    state,
                    substate.clone(),
                    dpu_snapshot,
                    ctx,
                    &DpuInitNextStateResolver {},
                )
                .await
            }
            DpuInitState::Init => {
                // initial restart, firmware update and scout is run, first reboot of dpu discovery
                let dpu_discovery_result = try_wait_for_dpu_discovery(
                    state,
                    &self.reachability_params,
                    ctx,
                    false,
                    dpu_machine_id,
                )
                .await?;

                if let Some(dpu_id) = dpu_discovery_result {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "Waiting for DPU {dpu_id} discovery and reboot"
                    )));
                }

                tracing::debug!(
                    firmware_update_enabled = self.dpu_nic_firmware_initial_update_enabled,
                    "ManagedHostState::DPUNotReady::Init"
                );

                // All DPUs are discovered. Reboot them to proceed.
                for dpu_snapshot in &state.dpu_snapshots {
                    handler_restart_dpu(
                        dpu_snapshot,
                        ctx,
                        state.host_snapshot.config.dpf.used_for_ingestion,
                    )
                    .await?;
                }

                let machine_state = DpuInitState::WaitingForPlatformPowercycle {
                    substate: PerformPowerOperation::Off,
                };
                let next_state =
                    machine_state.next_state_with_all_dpus_updated(&state.managed_state)?;
                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuInitState::DpfStates { state: dpf_state } => {
                let dpf_sdk = self.dpf_sdk.as_deref().ok_or_else(|| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "DPF state reached but DPF is not configured"
                    ))
                })?;
                dpf::handle_dpf_state(
                    state,
                    dpu_snapshot,
                    dpf_state,
                    ctx,
                    dpf_sdk,
                    self.reachability_params.power_down_wait,
                )
                .await
            }
            DpuInitState::WaitingForPlatformPowercycle {
                substate: PerformPowerOperation::Off,
            } => {
                // Wait until all DPUs arrive in Off state.
                if !state.managed_state.all_dpu_states_in_sync()? {
                    return Ok(StateHandlerOutcome::wait(
                        "Waiting for all dpus to move to off state.".to_string(),
                    ));
                }

                handler_host_power_control(state, ctx, SystemPowerControl::ForceOff).await?;

                let next_state = DpuInitState::WaitingForPlatformPowerOff
                    .next_state_with_all_dpus_updated(&state.managed_state)?;

                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuInitState::WaitingForPlatformPowerOff => {
                self.handle_waiting_for_platform_power_off(state, ctx).await
            }
            DpuInitState::WaitingForPlatformPowercycle {
                substate: PerformPowerOperation::On,
            } => {
                let basetime = state
                    .host_snapshot
                    .status
                    .last_reboot_requested
                    .as_ref()
                    .map(|x| x.time)
                    .unwrap_or(state.host_snapshot.state.version.timestamp());

                if wait(&basetime, self.reachability_params.power_down_wait) {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "Waiting for power_down_wait ({}m) to elapse before powering on host",
                        self.reachability_params.power_down_wait.num_minutes(),
                    )));
                }

                handler_host_power_control(state, ctx, SystemPowerControl::On).await?;

                let next_state = DpuInitState::WaitingForPlatformConfiguration
                    .next_state_with_all_dpus_updated(&state.managed_state)?;

                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuInitState::WaitingForPlatformConfiguration => {
                // A known BMC MAC is a hard precondition for setting the DPU UEFI
                // password: it keys the dpu_uefi rotation bookkeeping recorded once
                // uefi_setup succeeds below, so refuse to drive the device without
                // it rather than discovering afterward that we cannot track it.
                let dpu_bmc_mac = dpu_snapshot.status.bmc_info.mac.ok_or_else(|| {
                    StateHandlerError::MissingData {
                        object_id: dpu_snapshot.id.to_string(),
                        missing: "bmc_mac",
                    }
                })?;

                let dpf_managed_bf4 = is_dpf_managed_bf4(state, dpu_snapshot)?;

                // One access-info lookup serves both the general client
                // and the credential op below.
                let client_result = async {
                    let access = ctx
                        .services
                        .bmc_access_info_for_machine(dpu_snapshot)
                        .await?;
                    let client = ctx
                        .services
                        .redfish_client_pool
                        .client_by_info(&access)
                        .await
                        .map_err(StateHandlerError::from)?;
                    Ok::<_, StateHandlerError>((access, client))
                }
                .await;
                let (dpu_bmc_access, dpu_redfish_client) = match client_result {
                    Ok(v) => v,
                    Err(e) => {
                        let msg = format!(
                            "failed to create redfish client for DPU {}, potentially because we turned the host off as part of error handling in this state. err: {}",
                            dpu_snapshot.id, e
                        );
                        tracing::warn!(msg);
                        // If we cannot create a redfish client for the DPU, this function call will never result in an actual DPU reboot.
                        // The only side effect is turning the DPU's host back on if we turned it off earlier.
                        let reboot_status = trigger_reboot_if_needed(
                            dpu_snapshot,
                            state,
                            None,
                            &self.reachability_params,
                            ctx,
                        )
                        .await?;

                        return Ok(StateHandlerOutcome::wait(format!(
                            "{msg};\nDPU reboot status: {reboot_status:#?}",
                        )));
                    }
                };

                // fixme: in case of DPF ingested machine, the fw version compare should be done
                // with the image with which the ingestion is done.
                if !state.host_snapshot.config.dpf.used_for_ingestion
                    && let Some(outcome) = check_fw_component_version(
                        ctx,
                        dpu_snapshot,
                        &self.hardware_models.create_snapshot(),
                    )
                    .await?
                {
                    return Ok(outcome);
                }

                // DPF owns BF4 platform configuration. NICo still owns the
                // UEFI credential change performed below until DPF credential
                // management is available.
                if !dpf_managed_bf4 {
                    let boot_interface = None; // libredfish will choose the DPU
                    if self.enable_secure_boot {
                        dpu_redfish_client
                            .set_host_rshim(EnabledDisabled::Disabled)
                            .await
                            .map_err(|e| redfish_error("set_host_rshim", e))?;
                        dpu_redfish_client
                            .set_host_privilege_level(HostPrivilegeLevel::Restricted)
                            .await
                            .map_err(|e| redfish_error("set_host_privilege_level", e))?;
                    } else if let Err(e) = call_machine_setup_and_handle_no_dpu_error(
                        dpu_redfish_client.as_ref(),
                        boot_interface,
                        state.host_snapshot.associated_dpu_machine_ids().len(),
                        &ctx.services.site_config,
                    )
                    .await
                    {
                        // TODO(chet): I don't know if this is still a thing, but I'm pretty sure
                        // it hasn't been fixed/addressed yet, and it appears to be logged all over
                        // the place now.
                        let msg = format!(
                            "redfish machine_setup failed for DPU {}, potentially due to known race condition between UEFI POST and BMC. issuing a force-restart. err: {}",
                            dpu_snapshot.id, e
                        );
                        tracing::warn!(msg);
                        let reboot_status = trigger_reboot_if_needed(
                            dpu_snapshot,
                            state,
                            None,
                            &self.reachability_params,
                            ctx,
                        )
                        .await?;

                        return Ok(StateHandlerOutcome::wait(format!(
                            "{msg};\nWaiting for DPU {} to reboot: {reboot_status:#?}",
                            dpu_snapshot.id
                        )));
                    }
                }

                let dpu_uefi_credentials = resolve_site_uefi_credentials(
                    &ctx.services.db_pool,
                    ctx.services.bmc_credential_ops.credential_reader(),
                    db::credential_rotation::CredentialRotationType::DpuUefi,
                )
                .await?;
                let credentials_updated = match ctx
                    .services
                    .bmc_credential_ops
                    .uefi_setup(&dpu_bmc_access, true, dpu_uefi_credentials)
                    .await
                {
                    Err(e) => {
                        let msg = format!(
                            "Failed to run uefi_setup call failed for DPU {}: {}",
                            dpu_snapshot.id, e
                        );
                        tracing::warn!(
                            machine_id = %dpu_snapshot.id,
                            error = %e,
                            "Failed to run uefi_setup for DPU",
                        );
                        let reboot_status = trigger_reboot_if_needed(
                            dpu_snapshot,
                            state,
                            None,
                            &self.reachability_params,
                            ctx,
                        )
                        .await?;

                        return Ok(StateHandlerOutcome::wait(format!(
                            "{msg};\nWaiting for DPU {} to reboot: {reboot_status:#?}",
                            dpu_snapshot.id
                        )));
                    }
                    // BIOS does not expose a UEFI password field (e.g. BF4); skip
                    // the restart and convergence record — nothing was staged.
                    Ok(None) => false,
                    // Password staged through Redfish BIOS settings; restart to commit.
                    Ok(Some(_)) => true,
                };

                if credentials_updated {
                    handler_restart_dpu(
                        dpu_snapshot,
                        ctx,
                        state.host_snapshot.config.dpf.used_for_ingestion,
                    )
                    .await?;
                }

                let next_state = if dpf_managed_bf4 {
                    DpuInitState::WaitingForNetworkConfig
                } else {
                    DpuInitState::PollingBiosSetup
                }
                .next_state(&state.managed_state, dpu_machine_id)?;

                if credentials_updated {
                    // The DPU's UEFI password is now the site-wide value (just set via
                    // uefi_setup above): record dpu_uefi convergence so the rotation
                    // engine tracks this DPU from ingestion onward (mirrors the
                    // backfill, which keys DPU UEFI by the DPU BMC MAC). The MAC was
                    // validated as a precondition at the top of this state. Committed
                    // with the state transition below.
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db::credential_rotation::record_device_converged(
                        &mut txn,
                        dpu_bmc_mac,
                        db::credential_rotation::CredentialRotationType::DpuUefi,
                    )
                    .await
                    .map_err(|e| {
                        StateHandlerError::GenericError(eyre!(
                            "record dpu_uefi credential rotation convergence failed: {}",
                            e
                        ))
                    })?;
                    Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
                } else {
                    Ok(StateHandlerOutcome::transition(next_state))
                }
            }

            DpuInitState::PollingBiosSetup => {
                let next_state = DpuInitState::WaitingForNetworkConfig
                    .next_state(&state.managed_state, dpu_machine_id)?;

                // Handle BF4 machines already persisted in this state when the
                // controller is upgraded: do not issue even one more BIOS query.
                if is_dpf_managed_bf4(state, dpu_snapshot)? {
                    tracing::info!(
                        dpu_machine_id = %dpu_snapshot.id,
                        "Skipping BIOS setup verification for DPF-managed BF4"
                    );
                    return Ok(StateHandlerOutcome::transition(next_state));
                }

                let dpu_redfish_client = match ctx
                    .services
                    .create_redfish_client_from_machine(dpu_snapshot)
                    .await
                {
                    Ok(client) => client,
                    Err(e) => {
                        return Err(redfish_error(
                            "create_client_from_machine",
                            RedfishError::GenericError {
                                error: e.to_string(),
                            },
                        ));
                    }
                };

                match dpu_redfish_client.is_bios_setup(None).await {
                    Ok(true) => {
                        tracing::info!(
                            dpu_machine_id = %dpu_snapshot.id,
                            "BIOS setup verified"
                        );
                        Ok(StateHandlerOutcome::transition(next_state))
                    }
                    Ok(false) => Ok(StateHandlerOutcome::wait(format!(
                        "Polling BIOS setup status, waiting for settings to be applied on DPU {}",
                        dpu_snapshot.id
                    ))),
                    Err(e)
                        if carbide_redfish::libredfish::dpu_bios::is_dpu_bios_attributes_not_ready(&e) =>
                    {
                        let msg = format!(
                            "DPU {} BIOS attributes not ready ({e}); issuing a force-restart to mitigate the known UEFI POST/BMC race",
                            dpu_snapshot.id
                        );
                        tracing::warn!(
                            machine_id = %dpu_snapshot.id,
                            error = %e,
                            "DPU BIOS attributes not ready; issuing a force-restart to mitigate the known UEFI POST/BMC race",
                        );
                        let reboot_status = trigger_reboot_if_needed(
                            dpu_snapshot,
                            state,
                            None,
                            &self.reachability_params,
                            ctx,
                        )
                        .await?;

                        Ok(StateHandlerOutcome::wait(format!(
                            "{msg};\nWaiting for DPU {} to reboot: {reboot_status:#?}",
                            dpu_snapshot.id
                        )))
                    }
                    Err(e) => {
                        tracing::warn!(
                            dpu_machine_id = %dpu_snapshot.id,
                            error = %e,
                            "Failed to check DPU BIOS setup status, will retry"
                        );
                        Ok(StateHandlerOutcome::wait(format!(
                            "Failed to check BIOS setup status for DPU {}: {}. Will retry.",
                            dpu_snapshot.id, e
                        )))
                    }
                }
            }

            DpuInitState::WaitingForNetworkConfig => {
                // is_network_ready is syncing over all DPUs.
                // The code will move only when all DPUs returns network_ready signal.
                for dsnapshot in &state.dpu_snapshots {
                    if !managed_host_network_config_version_synced_and_dpu_healthy(
                        dsnapshot,
                        state.host_snapshot.network_config.version,
                    ) {
                        let mut reboot_status = None;
                        // Only reboot the DPU which is targeted in this event loop.
                        if dsnapshot.id == dpu_snapshot.id {
                            // we requested a DPU reboot in DpuInitState::Init
                            // let the trigger_reboot_if_needed determine if we are stuck here
                            // (based on how long it has been since the last requested reboot)
                            reboot_status = Some(
                                trigger_reboot_if_needed(
                                    dsnapshot,
                                    state,
                                    None,
                                    &self.reachability_params,
                                    ctx,
                                )
                                .await?,
                            );
                        }

                        // TODO: Make is_network_ready give us more details as a string
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for DPU agent to apply network config and report healthy network for DPU {}\nreboot status: {reboot_status:#?}",
                            dsnapshot.id
                        )));
                    }
                }

                let next_state = ManagedHostState::HostInit {
                    machine_state: MachineState::EnableIpmiOverLan,
                };
                Ok(StateHandlerOutcome::transition(next_state))
            }
            DpuInitState::WaitingForNetworkInstall => {
                tracing::warn!(
                    machine_id = %dpu_machine_id,
                    "Invalid State WaitingForNetworkInstall for dpu Machine"
                );
                Err(StateHandlerError::InvalidHostState(
                    (*dpu_machine_id).into(),
                    Box::new(state.managed_state.clone()),
                ))
            }
        }
    }

    async fn set_secure_boot(
        &self,
        count: u32,
        state: &ManagedHostStateSnapshot,
        set_secure_boot_state: SetSecureBootState,
        enable_secure_boot: bool,
        dpu_snapshot: &Machine,
        dpu_redfish_client: &dyn Redfish,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let next_state: ManagedHostState;
        let dpu_machine_id = dpu_snapshot.dpu_machine_id()?;
        let dpu_machine_id = &dpu_machine_id;

        // Use the host snapshot instead of the DPU snapshot because
        // the state.host_snapshot.current.version might be a bit more correct:
        // the state machine is driven by the host state
        let time_since_state_change: chrono::TimeDelta =
            state.host_snapshot.state.version.since_state_change();

        let wait_for_dpu_to_come_up = if time_since_state_change.num_minutes() > 5 {
            false
        } else {
            let (has_dpu_finished_booting, dpu_boot_progress) =
                did_dpu_finish_booting(dpu_redfish_client)
                    .await
                    .map_err(|e| redfish_error("did_dpu_finish_booting", e))?;

            if count > 0 && !has_dpu_finished_booting {
                tracing::info!(
                    machine_id = %dpu_snapshot.id,
                    boot_progress = ?dpu_boot_progress,
                    attempt = count,
                    "Waiting for DPU to finish booting; SetSecureBoot cycle"
                )
            }

            !has_dpu_finished_booting
        };

        match set_secure_boot_state {
            SetSecureBootState::WaitCertificateUpload { task_id } => {
                let task = dpu_redfish_client
                    .get_task(task_id.as_str())
                    .await
                    .map_err(|e| redfish_error("get_task", e))?;
                match task.clone().task_state {
                    Some(TaskState::New)
                    | Some(TaskState::Starting)
                    | Some(TaskState::Running)
                    | Some(TaskState::Pending) => {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for certificate upload task {task_id} to complete",
                        )));
                    }
                    Some(TaskState::Completed) => {
                        next_state = DpuDiscoveringState::EnableSecureBoot {
                            enable_secure_boot_state: SetSecureBootState::SetSecureBoot,
                            count: 0,
                        }
                        .next_state(&state.managed_state, dpu_machine_id)?;
                    }
                    None => {
                        return Err(redfish_error("get_task", RedfishError::NoContent));
                    }
                    Some(e) => {
                        return Err(redfish_error(
                            "get_task",
                            RedfishError::GenericError {
                                error: format!("Task {task:#?} error: {e:#?}"),
                            },
                        ));
                    }
                }
            }
            SetSecureBootState::CheckSecureBootStatus => {
                // This is the logic:
                // CheckSecureBootStatus -> DisableSecureBoot -> DisableSecureBootState::RebootDPU{0} -> DisableSecureBootState::RebootDPU{1}
                // The first time we check to see if secure boot is disabled, we do not need to wait. The DPU should already be up.
                // However, we need to give time in between the second reboot and checking the status again.
                if count > 0 && wait_for_dpu_to_come_up {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "Waiting for DPU {dpu_machine_id} to come back up from last reboot; time since last reboot: {time_since_state_change}; DisableSecureBoot cycle: {count}",
                    )));
                }

                match self
                    .is_secure_boot_disabled(dpu_machine_id, dpu_redfish_client)
                    .await
                {
                    Ok(is_secure_boot_disabled) if !enable_secure_boot => {
                        if is_secure_boot_disabled {
                            next_state = DpuDiscoveringState::SetUefiHttpBoot
                                .next_state(&state.managed_state, dpu_machine_id)?;
                        } else {
                            next_state = DpuDiscoveringState::DisableSecureBoot {
                                disable_secure_boot_state: Some(SetSecureBootState::SetSecureBoot),
                                count,
                            }
                            .next_state(&state.managed_state, dpu_machine_id)?;
                        }
                    }
                    Ok(is_secure_boot_disabled) => {
                        if is_secure_boot_disabled {
                            let pk_certs = dpu_redfish_client
                                .get_secure_boot_certificates("PK")
                                .await
                                .map_err(|e| redfish_error("get_secure_boot_certificates", e))?;

                            if pk_certs.is_empty() {
                                let mut cert_file = File::open("/forge-boot-artifacts/blobs/internal/aarch64/secure-boot-pk.pem").await.map_err(|e| redfish_error("open_secure_boot_certificate_file", RedfishError::FileError(format!("Error opening secure boot certificate file: {e}"))))?;
                                let mut cert_string = String::new();
                                cert_file
                                    .read_to_string(&mut cert_string)
                                    .await
                                    .map_err(|e| {
                                        redfish_error(
                                            "read_secure_boot_certificate_file",
                                            RedfishError::FileError(format!(
                                                "Error reading secure boot certificate file: {e}"
                                            )),
                                        )
                                    })?;
                                let task = dpu_redfish_client
                                    .add_secure_boot_certificate(cert_string.as_str(), "PK")
                                    .await
                                    .map_err(|e| redfish_error("add_secure_boot_certificate", e))?;
                                dpu_redfish_client
                                    .power(SystemPowerControl::ForceRestart)
                                    .await
                                    .map_err(|e| redfish_error("force_restart", e))?;
                                next_state = DpuDiscoveringState::EnableSecureBoot {
                                    enable_secure_boot_state:
                                        SetSecureBootState::WaitCertificateUpload {
                                            task_id: task.id,
                                        },
                                    count: 0,
                                }
                                .next_state(&state.managed_state, dpu_machine_id)?;
                            } else {
                                next_state = DpuDiscoveringState::EnableSecureBoot {
                                    enable_secure_boot_state: SetSecureBootState::SetSecureBoot,
                                    count,
                                }
                                .next_state(&state.managed_state, dpu_machine_id)?;
                            }
                        } else {
                            next_state = DpuInitState::InstallDpuOs {
                                substate: InstallDpuOsState::InstallingBFB,
                            }
                            .next_state(&state.managed_state, dpu_machine_id)?;
                        }
                    }
                    Err(StateHandlerError::MissingData { object_id, missing }) => {
                        tracing::info!(
                            machine_id = %object_id,
                            missing = %missing,
                            "Missing data in secure boot status response for DPU; rebooting DPU as a work-around"
                        );

                        /***
                         * If the DPU's BMC comes up after UEFI client was run on an ARM
                         * there is a known issue where the redfish query for the secure boot
                         * status comes back incomplete.
                         * Example:
                         * {
                                "@odata.id": "/redfish/v1/Systems/Bluefield/SecureBoot",
                                "@odata.type": "#SecureBoot.v1_1_0.SecureBoot",
                                "Description": "The UEFI Secure Boot associated with this system.",
                                "Id": "SecureBoot",
                                "Name": "UEFI Secure Boot",
                                "SecureBootDatabases": {
                                    "@odata.id": "/redfish/v1/Systems/Bluefield/SecureBoot/SecureBootDatabases"
                            }

                        (missing the SecureBootEnable and SecureBootCurrentBoot fields)
                        The known work around for this issue is to reboot the DPU's ARM. There is a pending FR
                        to fix this on the hardware level.
                        ***/

                        // Do not reboot the DPU indefinitely, something else might be wrong (DPU might be bust).
                        if count < 10 {
                            dpu_redfish_client
                                .power(SystemPowerControl::ForceRestart)
                                .await
                                .map_err(|e| redfish_error("force_restart", e))?;
                            if enable_secure_boot {
                                next_state = DpuDiscoveringState::EnableSecureBoot {
                                    enable_secure_boot_state: SetSecureBootState::RebootDPU {
                                        reboot_count: 0,
                                    },
                                    count: count + 1,
                                }
                                .next_state(&state.managed_state, dpu_machine_id)?;
                            } else {
                                next_state = DpuDiscoveringState::DisableSecureBoot {
                                    disable_secure_boot_state: Some(
                                        SetSecureBootState::CheckSecureBootStatus,
                                    ),
                                    count: count + 1,
                                }
                                .next_state(&state.managed_state, dpu_machine_id)?;
                            }
                        } else {
                            return Err(StateHandlerError::MissingData { object_id, missing });
                        }
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
            SetSecureBootState::DisableSecureBoot | SetSecureBootState::SetSecureBoot => {
                if enable_secure_boot {
                    dpu_redfish_client
                        .enable_secure_boot()
                        .await
                        .map_err(|e| redfish_error("enable_secure_boot", e))?;

                    next_state = DpuDiscoveringState::EnableSecureBoot {
                        enable_secure_boot_state: SetSecureBootState::RebootDPU { reboot_count: 0 },
                        count,
                    }
                    .next_state(&state.managed_state, dpu_machine_id)?;
                } else {
                    dpu_redfish_client
                        .disable_secure_boot()
                        .await
                        .map_err(|e| redfish_error("disable_secure_boot", e))?;

                    next_state = DpuDiscoveringState::DisableSecureBoot {
                        disable_secure_boot_state: Some(SetSecureBootState::RebootDPU {
                            reboot_count: 0,
                        }),
                        count,
                    }
                    .next_state(&state.managed_state, dpu_machine_id)?;
                }
            }
            // DPUs requires two reboots after the previous step in order to disable secure boot.
            // From the doc linked above: "the BlueField Arm OS must be rebooted twice. The first
            // reboot is for the UEFI redfish client to read the request from the BMC and apply it; the
            // second reboot is for the setting to take effect."
            // We do not need to wait between disabling secure boot and the first reboot.
            // But, we need to give the DPU time to come up after the initial reboot,
            // before we reboot it again.
            SetSecureBootState::RebootDPU { reboot_count } => {
                if reboot_count == 0 {
                    next_state = if enable_secure_boot {
                        DpuDiscoveringState::EnableSecureBoot {
                            enable_secure_boot_state: SetSecureBootState::RebootDPU {
                                reboot_count: reboot_count + 1,
                            },
                            count,
                        }
                        .next_state(&state.managed_state, dpu_machine_id)?
                    } else {
                        DpuDiscoveringState::DisableSecureBoot {
                            disable_secure_boot_state: Some(SetSecureBootState::RebootDPU {
                                reboot_count: reboot_count + 1,
                            }),
                            count,
                        }
                        .next_state(&state.managed_state, dpu_machine_id)?
                    };
                } else {
                    if wait_for_dpu_to_come_up {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for DPU {dpu_machine_id} to come back up from last reboot; time since last reboot: {time_since_state_change}",
                        )));
                    }
                    if enable_secure_boot {
                        next_state = DpuDiscoveringState::EnableSecureBoot {
                            enable_secure_boot_state: SetSecureBootState::CheckSecureBootStatus,
                            count: count + 1,
                        }
                        .next_state(&state.managed_state, dpu_machine_id)?;
                    } else {
                        next_state = DpuDiscoveringState::DisableSecureBoot {
                            disable_secure_boot_state: Some(
                                SetSecureBootState::CheckSecureBootStatus,
                            ),
                            count: count + 1,
                        }
                        .next_state(&state.managed_state, dpu_machine_id)?;
                    }
                }

                dpu_redfish_client
                    .power(SystemPowerControl::ForceRestart)
                    .await
                    .map_err(|e| redfish_error("force_restart", e))?;
            }
        }

        Ok(StateHandlerOutcome::transition(next_state))
    }

    /// Waits for the one host-wide `ForceOff` to become visible through Redfish.
    ///
    /// Normal dispatch calls this before walking individual DPUs so a
    /// multi-DPU host performs one BMC read per controller iteration.
    async fn handle_waiting_for_platform_power_off(
        &self,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        // Redfish power actions are asynchronous. Persist this wait before
        // trusting a new reading so stale `On` cannot skip the power cycle.
        if !is_host_powered_off(state, ctx).await? {
            return Ok(StateHandlerOutcome::wait(format!(
                "Waiting for host {} to power off before powering it on",
                state.host_snapshot.id,
            )));
        }

        let next_state = DpuInitState::WaitingForPlatformPowercycle {
            substate: PerformPowerOperation::On,
        }
        .next_state_with_all_dpus_updated(&state.managed_state)?;

        Ok(StateHandlerOutcome::transition(next_state))
    }
}

#[async_trait::async_trait]
impl StateHandler for DpuMachineStateHandler {
    type State = ManagedHostStateSnapshot;
    type ControllerState = ManagedHostState;
    type ObjectId = MachineId;
    type ContextObjects = MachineStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        _host_machine_id: &MachineId,
        state: &mut ManagedHostStateSnapshot,
        _controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let mut state_handler_outcome = StateHandlerOutcome::do_nothing();
        if state.host_snapshot.associated_dpu_machine_ids().is_empty() {
            let next_state = ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
            };
            Ok(StateHandlerOutcome::transition(next_state))
        } else {
            if let ManagedHostState::DPUInit { dpu_states } = &state.managed_state
                && !dpu_states.states.is_empty()
                && dpu_states
                    .states
                    .values()
                    .all(|state| matches!(state, DpuInitState::WaitingForPlatformPowerOff))
            {
                return self.handle_waiting_for_platform_power_off(state, ctx).await;
            }

            for dpu_snapshot in &state.dpu_snapshots {
                state_handler_outcome = self.handle_dpuinit_state(state, dpu_snapshot, ctx).await?;

                if let outcome @ StateHandlerOutcome::Transition { .. } = state_handler_outcome {
                    return Ok(outcome);
                }
            }

            Ok(state_handler_outcome)
        }
    }
}

fn get_reboot_cycle(
    next_potential_reboot_time: DateTime<Utc>,
    entered_state_at: DateTime<Utc>,
    wait_period: Duration,
) -> Result<i64, StateHandlerError> {
    if next_potential_reboot_time <= entered_state_at {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "poorly configured paramters: next_potential_reboot_time: {}, entered_state_at: {}, wait_period: {}",
            next_potential_reboot_time,
            entered_state_at,
            wait_period.num_minutes()
        )));
    }

    let cycle = next_potential_reboot_time - entered_state_at;

    // Although trigger_reboot_if_needed makes sure to not send wait_period as 0, but still if some other
    // function calls get_reboot_cycle, this function must not panic, so setting it min 1 minute
    // here as well.
    Ok(cycle.num_minutes() / wait_period.num_minutes().max(1))
}

#[derive(Debug)]
pub struct RebootStatus {
    increase_retry_count: bool, // the vague previous return value
    status: String,             // what we did or are waiting for
}

/// Outcome of set_host_boot_order function.
enum SetBootOrderOutcome {
    Continue(SetBootOrderInfo),
    /// BIOS drifted after the caller's inspection; return to the shared BIOS
    /// configuration stages so any Redfish job ID is persisted and polled.
    ConfigureBios,
    Done,
    WaitingForReboot(String),
    /// No boot interface to act on yet -- e.g. a zero-DPU host whose boot NIC
    /// has not been discovered. Distinct from `WaitingForReboot`: nothing was
    /// rebooted, the caller just waits and retries.
    Wait(String),
}

/// Outcome of [`require_boot_interface`]: the resolved boot NIC, or the
/// caller's wait outcome for a zero-DPU host still discovering its boot NIC.
#[derive(Debug)]
enum RequiredBootInterface<W> {
    Ready(BootInterfaceTarget),
    Wait(W),
}

/// Map a boot-interface resolution for Redfish steps that require a target.
///
/// A zero-DPU host that has not discovered its boot NIC maps to the caller's
/// wait outcome (`wait` wraps the shared message and `activity` names the
/// blocked step). A host that should already have an interface returns an
/// error.
fn require_boot_interface<W>(
    host_id: &MachineId,
    resolution: BootInterfaceResolution,
    activity: &str,
    wait: impl FnOnce(String) -> W,
) -> Result<RequiredBootInterface<W>, StateHandlerError> {
    match resolution {
        BootInterfaceResolution::Ready(target) => Ok(RequiredBootInterface::Ready(target)),
        BootInterfaceResolution::AwaitingNic => Ok(RequiredBootInterface::Wait(wait(format!(
            "Waiting for zero-DPU host {host_id} to discover its boot NIC before {activity}."
        )))),
        BootInterfaceResolution::Missing => Err(StateHandlerError::GenericError(eyre::eyre!(
            "missing boot interface for host: {host_id}"
        ))),
    }
}

#[cfg(test)]
mod require_boot_interface_tests {
    use super::*;

    fn host_id() -> MachineId {
        "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng"
            .parse()
            .unwrap()
    }

    // Ready passes the resolved target through untouched.
    #[test]
    fn ready_passes_the_target_through() {
        let target = BootInterfaceTarget::MacOnly("20:00:00:00:00:01".parse().unwrap());
        let resolved = require_boot_interface::<String>(
            &host_id(),
            BootInterfaceResolution::Ready(target.clone()),
            "setting boot order",
            |msg| msg,
        )
        .unwrap();
        let RequiredBootInterface::Ready(out) = resolved else {
            panic!("expected Ready");
        };
        assert_eq!(out, target);
    }

    // AwaitingNic becomes the caller's wait outcome, built from the shared
    // message with the caller's activity spliced in.
    #[test]
    fn awaiting_nic_maps_to_the_callers_wait_outcome() {
        let resolved = require_boot_interface::<String>(
            &host_id(),
            BootInterfaceResolution::AwaitingNic,
            "setting boot order",
            |msg| msg,
        )
        .unwrap();
        let RequiredBootInterface::Wait(msg) = resolved else {
            panic!("expected Wait");
        };
        assert_eq!(
            msg,
            format!(
                "Waiting for zero-DPU host {} to discover its boot NIC before setting boot order.",
                host_id()
            )
        );
    }

    // Missing is a hard error, not a wait.
    #[test]
    fn missing_is_a_hard_error() {
        let err = require_boot_interface::<String>(
            &host_id(),
            BootInterfaceResolution::Missing,
            "setting boot order",
            |msg| msg,
        )
        .unwrap_err();
        assert!(matches!(err, StateHandlerError::GenericError(_)));
    }
}

/// `trigger_reboot_if_needed` retries a machine that does not come up within the configured period,
/// for at most 15 periods. Callers that handle a recoverable error should ensure an attempt
/// timestamp is queued and return `Wait` or `DoNothing` so it is committed; returning an error can
/// cause another reboot on the next retry pass.
#[track_caller]
pub fn trigger_reboot_if_needed(
    target: &Machine,
    state: &ManagedHostStateSnapshot,
    retry_count: Option<i64>,
    reachability_params: &ReachabilityParams,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> impl Future<Output = Result<RebootStatus, StateHandlerError>> {
    let trigger_location = std::panic::Location::caller();
    trigger_reboot_if_needed_with_policy(
        target,
        state,
        retry_count,
        reachability_params,
        ctx,
        trigger_location,
        true,
    )
}

#[track_caller]
fn trigger_reboot_if_needed_without_power_cycle(
    target: &Machine,
    state: &ManagedHostStateSnapshot,
    retry_count: Option<i64>,
    reachability_params: &ReachabilityParams,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> impl Future<Output = Result<RebootStatus, StateHandlerError>> {
    let trigger_location = std::panic::Location::caller();
    trigger_reboot_if_needed_with_policy(
        target,
        state,
        retry_count,
        reachability_params,
        ctx,
        trigger_location,
        false,
    )
}

/// Queues a fresh retry timestamp after earlier writes and disables generic restart verification.
/// The verification write stores the supplied reboot record in full, so the next retry is
/// calculated from the updated time.
fn record_provisioning_retry_attempt(
    target: &Machine,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) {
    let mut current_reboot = target.status.last_reboot_requested.unwrap_or_default();
    current_reboot.time = Utc::now();

    ctx.pending_db_writes
        .push(MachineWriteOp::UpdateRestartVerificationStatus {
            machine_id: target.id,
            current_reboot,
            verified: None,
            attempts: 0,
        });
}

async fn trigger_reboot_if_needed_with_policy(
    target: &Machine,
    state: &ManagedHostStateSnapshot,
    retry_count: Option<i64>,
    reachability_params: &ReachabilityParams,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    trigger_location: &std::panic::Location<'_>,
    allow_power_cycle: bool,
) -> Result<RebootStatus, StateHandlerError> {
    let host = &state.host_snapshot;
    // Its highly unlikely that the host has never been rebooted (and the last_reboot_reqeusted
    // field shouldn't get cleared), but default it if its not set
    let last_reboot_requested = match &target.status.last_reboot_requested {
        None => &MachineLastRebootRequested {
            time: host.state.version.timestamp(),
            mode: MachineLastRebootRequestedMode::Reboot,
            ..MachineLastRebootRequested::default()
        },
        Some(req) => req,
    };

    if let MachineLastRebootRequestedMode::PowerOff = last_reboot_requested.mode {
        // PowerOn the host.
        tracing::info!(
            machine_id = %target.id,
            host_machine_id = %host.id,
            "Machine is in power-off state. Turning on for host",
        );

        if wait(
            &last_reboot_requested.time,
            reachability_params.power_down_wait,
        ) {
            return Ok(RebootStatus {
                increase_retry_count: false,
                status: format!(
                    "Waiting for host to power off. Next check at {}",
                    last_reboot_requested.time + reachability_params.power_down_wait
                ),
            });
        }

        let redfish_client = ctx
            .services
            .create_redfish_client_from_machine(host)
            .await?;

        let power_state = host_power_state(redfish_client.as_ref()).await?;

        // If power-off done, power-on now.
        // If host is not powered-off yet, try again.
        let action = if power_state == libredfish::PowerState::Off {
            SystemPowerControl::On
        } else {
            tracing::error!(
                machine_id = %target.id,
                host_machine_id = %host.id,
                "Machine is still not power-off state. Turning off again for host",
            );
            SystemPowerControl::ForceOff
        };

        tracing::trace!(
            machine_id = %target.id,
            %action,
            "Redfish setting host power state"
        );
        handler_host_power_control_with_location(state, ctx, action, trigger_location).await?;
        return Ok(RebootStatus {
            increase_retry_count: false,
            status: format!("Set power state to {action} using Redfish API"),
        });
    }

    // Check if reboot is prevented by health override.
    if state.aggregate_health.is_reboot_blocked_in_state_machine() {
        tracing::info!(
            machine_id = %target.id,
            "Not trying to reboot since health override is set to prevent reboot.",
        );
        return Ok(RebootStatus {
            increase_retry_count: false,
            status: format!(
                "Not trying to reboot {} since health override is set to prevent reboot.",
                target.id
            ),
        });
    }

    let wait_period = reachability_params
        .failure_retry_time
        .max(Duration::minutes(1));

    let current_time = Utc::now();
    let entered_state_at = target.state.version.timestamp();
    let next_potential_reboot_time: DateTime<Utc> =
        if last_reboot_requested.time + wait_period > entered_state_at {
            last_reboot_requested.time + wait_period
        } else {
            // Handles this case:
            // T0: State A
            //      DPU was hung--Reboot DPU
            //      DPU last requested reboot requested time: T0
            // T1 (T0 + 1 hour): State B
            //      DPU was hung; DPU wait period is 45 mins
            //      If we only calculate the next reboot time from the last requested reboot time
            //      the DPU's next potential reboot time = T0 + 45 < T1
            // Our logic to detect the reboot cycle will return an error here,
            // because the next reboot time is before the time the DPU entered State B.
            // Update the DPU's next reboot time to be 5 minutes after it entered State B to handle
            // this edge case.
            entered_state_at + Duration::minutes(5)
        };

    let time_elapsed_since_state_change = (current_time - entered_state_at).num_minutes();
    // Let's stop at 15 cycles of reboot.
    let max_retry_duration = Duration::minutes(wait_period.num_minutes() * 15);

    let should_try = if let Some(retry_count) = retry_count {
        retry_count < 15
    } else {
        entered_state_at + max_retry_duration > current_time
    };

    // We can try reboot only upto 15 cycles from state change.
    if should_try {
        // A cycle is done but host has not responded yet. Let's try a reboot.
        if next_potential_reboot_time < current_time {
            // Find the cycle.
            // We are trying to reboot 3 times and power down/up on 4th cycle.
            let cycle = match retry_count {
                Some(x) => x,
                None => {
                    get_reboot_cycle(next_potential_reboot_time, entered_state_at, wait_period)?
                }
            };

            // Dont power down the host on the first cycle
            let power_down_host = allow_power_cycle && cycle != 0 && cycle % 4 == 0;

            let status = if power_down_host {
                // PowerDown (or ACPowercycle for Lenovo)
                // DPU or host, in both cases power down is triggered from host.
                let vendor = state.host_snapshot.bmc_vendor();

                let action = if vendor.is_lenovo() {
                    SystemPowerControl::ACPowercycle
                } else {
                    SystemPowerControl::ForceOff
                };

                handler_host_power_control_with_location(state, ctx, action, trigger_location)
                    .await?;

                format!(
                    "{vendor} has not come up after {time_elapsed_since_state_change} minutes, trying {action}, cycle: {cycle}",
                )
            } else {
                // Reboot
                if target.id.machine_type().is_dpu() {
                    handler_restart_dpu(
                        target,
                        ctx,
                        state.host_snapshot.config.dpf.used_for_ingestion,
                    )
                    .await?;
                } else {
                    if let Ok(client) = ctx.services.create_redfish_client_from_machine(host).await
                    {
                        log_host_config(client.as_ref(), state).await;
                    }

                    handler_host_power_control_with_location(
                        state,
                        ctx,
                        SystemPowerControl::ForceRestart,
                        trigger_location,
                    )
                    .await?;
                }
                format!(
                    "Has not come up after {time_elapsed_since_state_change} minutes. Rebooting again, cycle: {cycle}."
                )
            };

            tracing::info!(
                machine_id = %target.id,
                managed_host_state = %state.managed_state,
                reboot_status = %status,
                "Triggered reboot in managed-host state",
            );

            Ok(RebootStatus {
                increase_retry_count: true,
                status,
            })
        } else {
            Ok(RebootStatus {
                increase_retry_count: false,
                status: format!("Will attempt next reboot at {next_potential_reboot_time}"),
            })
        }
    } else {
        let h = (current_time - entered_state_at).num_hours();
        Err(StateHandlerError::ManualInterventionRequired(format!(
            "Machine has not responded after {h} hours."
        )))
    }
}

/// This function waits until target machine is up or not. It relies on scout to identify if
/// machine has come up or not after reboot.
// True if machine is rebooted after state change.
pub fn rebooted(target: &Machine) -> bool {
    target.status.last_reboot_time.unwrap_or_default() > target.state.version.timestamp()
}

pub fn machine_validation_completed(target: &Machine) -> bool {
    target.last_machine_validation_time.unwrap_or_default() > target.state.version.timestamp()
}
// Was machine rebooted after state change?
fn discovered_after_state_transition(
    version: ConfigVersion,
    last_discovery_time: Option<DateTime<Utc>>,
) -> bool {
    last_discovery_time.unwrap_or_default() > version.timestamp()
}

// Was DPU reprov restart requested after state change
fn dpu_reprovision_restart_requested_after_state_transition(
    version: ConfigVersion,
    reprov_restart_requested_at: DateTime<Utc>,
) -> bool {
    reprov_restart_requested_at > version.timestamp()
}

fn cleanedup_after_state_transition(
    version: ConfigVersion,
    last_cleanup_time: Option<DateTime<Utc>>,
) -> bool {
    last_cleanup_time.unwrap_or_default() > version.timestamp()
}

fn waiting_for_cleanup_state(
    cleanup_state: CleanupState,
    cleanup_context: CleanupContext,
) -> ManagedHostState {
    ManagedHostState::WaitingForCleanup {
        cleanup_state,
        cleanup_context,
    }
}

fn initial_discovery_waiting_state() -> ManagedHostState {
    ManagedHostState::HostInit {
        machine_state: MachineState::WaitingForDiscovery,
    }
}

fn post_cleanup_state(cleanup_context: CleanupContext) -> ManagedHostState {
    match cleanup_context {
        CleanupContext::Deprovision => ManagedHostState::BomValidating {
            bom_validating_state: BomValidating::UpdatingInventory(BomValidatingContext {
                machine_validation_context: Some(MachineValidationContext::Cleanup),
                ..BomValidatingContext::default()
            }),
        },
        CleanupContext::InitialDiscovery => initial_discovery_waiting_state(),
    }
}

fn current_cleanup_context(
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<CleanupContext, StateHandlerError> {
    match &mh_snapshot.host_snapshot.state.value {
        ManagedHostState::WaitingForCleanup {
            cleanup_context, ..
        } => Ok(*cleanup_context),
        _ => Err(StateHandlerError::GenericError(eyre::eyre!(
            "unexpected host state for {}: {:#?}",
            mh_snapshot.host_snapshot.id,
            mh_snapshot.host_snapshot.state,
        ))),
    }
}

/// A `StateHandler` implementation for host machines
#[derive(Debug, Clone)]
pub struct HostMachineStateHandler {
    host_handler_params: HostHandlerParams,
}

impl HostMachineStateHandler {
    pub fn new(host_handler_params: HostHandlerParams) -> Self {
        Self {
            host_handler_params,
        }
    }
}

fn managed_host_network_config_version_synced_and_dpu_healthy(
    dpu_snapshot: &Machine,
    host_version: ConfigVersion,
) -> bool {
    if !dpu_snapshot.managed_host_network_config_version_synced(host_version) {
        return false;
    }

    let Some(dpu_health) = dpu_snapshot.dpu_agent_health_report() else {
        return false;
    };

    // Note that DPU alerts may be surpressed (classifications removed) in the aggregate health
    // report so the individual DPU's report is used.
    !dpu_health
        .has_classification(&health_report::HealthAlertClassification::prevent_host_state_changes())
}

fn check_host_health_for_alerts(state: &ManagedHostStateSnapshot) -> Result<(), StateHandlerError> {
    // In some states, DPU alerts may be surpressed (classifications removed) in the aggregate health report.
    // Since this is not called from a state that supresses DPU alerts, this is ok here.
    match state
        .aggregate_health
        .has_classification(&health_report::HealthAlertClassification::prevent_host_state_changes())
    {
        true => Err(StateHandlerError::HealthProbeAlert),
        false => Ok(()),
    }
}

const PRIMARY_DPU_BGP_WAIT_REASON: &str =
    "Waiting for the primary DPU p0 BGP session to be established";
const HOST_HEALTH_WAIT_REASON: &str =
    "Waiting for lifecycle-blocking host health alerts to clear before PXE reboot";
const DPU_NETWORK_READY_WAIT_REASON: &str =
    "Waiting for the primary DPU network to become ready before PXE reboot";

/// `provisioning_pxe_reboot_wait_reason` returns the safety condition blocking a provisioning PXE
/// reboot.
fn provisioning_pxe_reboot_wait_reason(
    state: &ManagedHostStateSnapshot,
) -> Result<Option<&'static str>, StateHandlerError> {
    match check_host_health_for_alerts(state) {
        Ok(()) => {}
        Err(StateHandlerError::HealthProbeAlert) => {
            return Ok(Some(HOST_HEALTH_WAIT_REASON));
        }
        Err(error) => return Err(error),
    }

    if state.has_managed_dpus() && primary_dpu_has_pxe_blocking_bgp_alert(state) {
        return Ok(Some(PRIMARY_DPU_BGP_WAIT_REASON));
    }

    Ok(None)
}

/// Returns whether the DPU agent marked a ToR BGP alert as unsafe for PXE allocation.
fn is_pxe_blocking_bgp_alert(alert: &HealthProbeAlert) -> bool {
    alert.id == HealthProbeId::bgp_peering_tor()
        && alert
            .classifications
            .contains(&HealthAlertClassification::prevent_allocations())
}

/// Checks whether a health report contains a BGP alert that should block PXE.
fn report_has_pxe_blocking_bgp_alert(report: &HealthReport) -> bool {
    report.alerts.iter().any(is_pxe_blocking_bgp_alert)
}

/// Returns the DPU attached to the host's primary interface.
fn primary_dpu_snapshot(state: &ManagedHostStateSnapshot) -> Option<&Machine> {
    let primary_dpu_id = state.host_snapshot.primary_attached_dpu_machine_id()?;

    state
        .dpu_snapshots
        .iter()
        .find(|dpu| dpu.id == MachineId::from(primary_dpu_id))
}

/// Returns whether a health alert shows that the primary DPU network is not ready for release PXE.
fn is_release_pxe_blocking_network_alert(alert: &HealthProbeAlert) -> bool {
    let network_health_probe_ids = [
        HealthProbeId::nvue_api_running(),
        HealthProbeId::bgp_stats(),
        HealthProbeId::post_config_check_wait(),
    ];

    (network_health_probe_ids.contains(&alert.id)
        && alert
            .classifications
            .contains(&HealthAlertClassification::prevent_allocations()))
        || is_pxe_blocking_bgp_alert(alert)
}

/// Checks whether a health report contains a network alert that should block release PXE.
fn report_has_release_pxe_blocking_network_alert(report: &HealthReport) -> bool {
    report
        .alerts
        .iter()
        .any(is_release_pxe_blocking_network_alert)
}

/// Checks host and primary DPU health reports using their effective precedence.
///
/// A host Replace report overrides all DPU reports. Otherwise, the primary
/// DPU's Replace report overrides its Merge reports, and any Merge report may
/// match when there is no Replace report.
fn primary_dpu_effective_health_matches(
    state: &ManagedHostStateSnapshot,
    report_matches: impl Fn(&HealthReport) -> bool,
) -> bool {
    if let Some(host_replace_report) = state.host_snapshot.health_reports.replace.as_ref() {
        return report_matches(host_replace_report);
    }

    let Some(primary_dpu) = primary_dpu_snapshot(state) else {
        return false;
    };

    if let Some(dpu_replace_report) = primary_dpu.health_reports.replace.as_ref() {
        return report_matches(dpu_replace_report);
    }

    primary_dpu
        .health_reports
        .merges
        .values()
        .any(report_matches)
}

/// Checks whether effective primary DPU health explicitly blocks release PXE.
///
/// Missing reports do not block instance deletion. The caller has already
/// required a fresh network status from every managed DPU, while an operator
/// Replace report retains its existing precedence over agent health.
fn primary_dpu_has_release_pxe_blocking_network_alert(state: &ManagedHostStateSnapshot) -> bool {
    primary_dpu_effective_health_matches(state, report_has_release_pxe_blocking_network_alert)
}

/// Checks whether effective health data contains the p0 BGP condition that blocks PXE.
///
/// The agent marks a failed p0 session with `PreventAllocations` because PXE
/// boot depends on p0.
fn primary_dpu_has_pxe_blocking_bgp_alert(state: &ManagedHostStateSnapshot) -> bool {
    primary_dpu_effective_health_matches(state, report_has_pxe_blocking_bgp_alert)
}

/// Whether a captured desired target can be replaced before this substate runs.
///
/// Pure checks and pre-write states can adopt newer intent. Vendor jobs,
/// polling, unlock choreography, and cleanup must finish against the captured
/// target so a concurrent write cannot orphan an in-flight Redfish operation.
fn ready_boot_config_can_adopt_latest(state: &ReadyBootConfigState) -> bool {
    match state {
        ReadyBootConfigState::Prepare
        | ReadyBootConfigState::CheckHostConfig
        | ReadyBootConfigState::ConfigureBios { .. } => true,
        ReadyBootConfigState::SetBootOrder {
            set_boot_order_info,
        } => matches!(
            set_boot_order_info.set_boot_order_state,
            SetBootOrderState::SetBootOrder | SetBootOrderState::CheckBootOrder
        ),
        ReadyBootConfigState::UnlockHost { .. }
        | ReadyBootConfigState::WaitingForBiosJob { .. }
        | ReadyBootConfigState::PollingBiosSetup { .. }
        | ReadyBootConfigState::LockHost { .. }
        | ReadyBootConfigState::Failed { .. } => false,
    }
}

// One complete persisted retry tolerates a transient final observation.
// Repeated drift after lockdown strongly suggests that lockdown or another
// actor is undoing the configuration.
const MAX_POST_LOCK_VERIFICATION_RETRIES: u32 = 1;

/// Returns the state required by newer intent, or `None` while the captured
/// desired version remains current.
fn ready_boot_config_superseded_state(
    captured_version: ConfigVersion,
    current_desired: Option<Versioned<MachineBootInterfaceTarget>>,
) -> Option<ManagedHostState> {
    if current_desired
        .as_ref()
        .is_some_and(|desired| desired.version == captured_version)
    {
        return None;
    }

    Some(current_desired.map_or(ManagedHostState::Ready, |desired| {
        ready_boot_configuring(desired, 0, ReadyBootConfigState::Prepare)
    }))
}

fn ready_boot_config_state_from_stage(stage: HostBootConfigStage) -> ReadyBootConfigState {
    match stage {
        HostBootConfigStage::ConfigureBios { retry_count } => {
            ReadyBootConfigState::ConfigureBios { retry_count }
        }
        HostBootConfigStage::WaitingForBiosJob { bios_config_info } => {
            ReadyBootConfigState::WaitingForBiosJob { bios_config_info }
        }
        HostBootConfigStage::PollingBiosSetup { retry_count } => {
            ReadyBootConfigState::PollingBiosSetup { retry_count }
        }
        HostBootConfigStage::SetBootOrder {
            set_boot_order_info,
        } => ReadyBootConfigState::SetBootOrder {
            set_boot_order_info,
        },
    }
}

/// Returns whether a Ready boot substate may have left lockdown disabled.
///
/// Keep this match exhaustive so a new substate cannot silently skip cleanup.
fn ready_boot_config_may_have_opened_lockdown(state: &ReadyBootConfigState) -> bool {
    match state {
        ReadyBootConfigState::Prepare
        | ReadyBootConfigState::LockHost { .. }
        | ReadyBootConfigState::Failed { .. } => false,
        ReadyBootConfigState::UnlockHost { .. }
        | ReadyBootConfigState::CheckHostConfig
        | ReadyBootConfigState::ConfigureBios { .. }
        | ReadyBootConfigState::WaitingForBiosJob { .. }
        | ReadyBootConfigState::PollingBiosSetup { .. }
        | ReadyBootConfigState::SetBootOrder { .. } => true,
    }
}

/// `ManagedHostState::BootConfiguring` must dispatch so `Prepare` can wait for
/// DPU snapshots or `LockHost` can restore lockdown.
fn ready_boot_config_handles_missing_dpu_snapshots(state: &ManagedHostState) -> bool {
    matches!(state, ManagedHostState::BootConfiguring { .. })
}

fn ready_boot_config_requires_timeout_cleanup(
    state: &ReadyBootConfigState,
    time_in_state: chrono::Duration,
) -> bool {
    let cleanup_timeout = chrono::Duration::from_std(model::machine::slas::BOOT_CONFIGURING)
        .expect("BootConfiguring SLA fits chrono::Duration");
    time_in_state >= cleanup_timeout && ready_boot_config_may_have_opened_lockdown(state)
}

fn ready_boot_configuring(
    desired: Versioned<MachineBootInterfaceTarget>,
    post_lock_verification_retry_count: u32,
    boot_config_state: ReadyBootConfigState,
) -> ManagedHostState {
    ManagedHostState::BootConfiguring {
        desired_version: desired.version,
        desired_boot_interface: desired.value,
        post_lock_verification_retry_count,
        boot_config_state,
    }
}

/// Builds the convergence state for a machine whose desired boot-interface
/// version has not yet been verified.
fn pending_ready_boot_config_state(machine: &Machine) -> Option<ManagedHostState> {
    let desired = machine.config.desired_boot_interface.as_ref()?;
    machine
        .pending_boot_interface_config_version()
        .map(|_| ready_boot_configuring(desired.clone(), 0, ReadyBootConfigState::Prepare))
}

fn ready_boot_config_locking(
    desired: Versioned<MachineBootInterfaceTarget>,
    post_lock_verification_retry_count: u32,
    post_lock_action: Option<ReadyBootConfigPostLockAction>,
) -> ManagedHostState {
    ready_boot_configuring(
        desired,
        post_lock_verification_retry_count,
        ReadyBootConfigState::LockHost { post_lock_action },
    )
}

fn ready_boot_config_after_post_lock_drift(
    desired: Versioned<MachineBootInterfaceTarget>,
    post_lock_verification_retry_count: u32,
    machine_id: MachineId,
) -> ManagedHostState {
    if post_lock_verification_retry_count < MAX_POST_LOCK_VERIFICATION_RETRIES {
        return ready_boot_configuring(
            desired,
            post_lock_verification_retry_count + 1,
            ReadyBootConfigState::Prepare,
        );
    }

    ready_boot_configuring(
        desired,
        post_lock_verification_retry_count,
        ReadyBootConfigState::Failed {
            failure: format!(
                "Boot configuration on host {machine_id} repeatedly drifted after lockdown was restored; manual intervention required"
            ),
        },
    )
}

/// Maps the shared HostInit boot driver onto persisted Ready substates.
///
/// Terminal driver failures pass through `LockHost` so convergence cannot park
/// while lockdown may be open.
async fn handle_ready_boot_config_stage(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    desired: Versioned<MachineBootInterfaceTarget>,
    post_lock_verification_retry_count: u32,
    stage: HostBootConfigStage,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let boot_interface: BootInterfaceTarget = desired.value.clone().into();
    let outcome = match run_host_boot_config_stage(
        ctx,
        reachability_params,
        redfish_client,
        mh_snapshot,
        Some(&boot_interface),
        stage,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(StateHandlerError::ManualInterventionRequired(failure)) => {
            return Ok(StateHandlerOutcome::transition(ready_boot_config_locking(
                desired,
                post_lock_verification_retry_count,
                Some(ReadyBootConfigPostLockAction::Convergence { failure }),
            )));
        }
        Err(error) => return Err(error),
    };

    match outcome {
        HostBootConfigOutcome::Continue(stage) => {
            Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                desired,
                post_lock_verification_retry_count,
                ready_boot_config_state_from_stage(stage),
            )))
        }
        HostBootConfigOutcome::Complete => Ok(StateHandlerOutcome::transition(
            ready_boot_config_locking(desired, post_lock_verification_retry_count, None),
        )),
        HostBootConfigOutcome::Wait(reason) => Ok(StateHandlerOutcome::wait(reason)),
        HostBootConfigOutcome::Failed { failure } => {
            Ok(StateHandlerOutcome::transition(ready_boot_config_locking(
                desired,
                post_lock_verification_retry_count,
                Some(ReadyBootConfigPostLockAction::Convergence { failure }),
            )))
        }
    }
}

/// Converges an unassigned Ready host to its persisted boot interface target.
///
/// The outer state captures one target and desired version. Safe boundaries
/// may adopt newer intent. An in-flight vendor job keeps its captured target;
/// if the DPU network status becomes stale within the cleanup deadline, the
/// controller restores lockdown and returns to `Prepare` before using that
/// target again. Work already beyond that deadline restores lockdown and parks.
async fn handle_ready_boot_config(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    desired: Versioned<MachineBootInterfaceTarget>,
    post_lock_verification_retry_count: u32,
    boot_config_state: ReadyBootConfigState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // `Prepare` has not changed the host yet, so an operator maintenance request
    // can safely take control even while a DPU is still catching up.
    if matches!(boot_config_state, ReadyBootConfigState::Prepare)
        && let Some(maintenance_transition) =
            maintenance::maintenance_transition_if_requested(mh_snapshot)
    {
        return Ok(maintenance_transition);
    }

    // Only states that can adopt replacement intent need an unlocked read.
    // `LockHost` reads again under the `Machine` row lock before it commits a
    // transition. An in-flight vendor stage keeps its captured target while
    // any required cleanup completes.
    let current_desired = if matches!(boot_config_state, ReadyBootConfigState::Failed { .. })
        || ready_boot_config_can_adopt_latest(&boot_config_state)
    {
        let mut conn = ctx.services.db_pool.acquire().await?;
        db::machine_desired_boot_interface::get(
            conn.as_mut(),
            &mh_snapshot.host_snapshot.id.try_into()?,
        )
        .await?
    } else {
        None
    };
    let captured_boot_interface: BootInterfaceTarget = desired.value.clone().into();
    let dpu_network_status_current = mh_snapshot.managed_host_network_config_version_synced();

    if ready_boot_config_requires_timeout_cleanup(
        &boot_config_state,
        mh_snapshot.host_snapshot.state.version.since_state_change(),
    ) {
        let failure = format!(
            "boot configuration convergence stopped progressing in {boot_config_state:?} for longer than the {} second BootConfiguring cleanup deadline",
            model::machine::slas::BOOT_CONFIGURING.as_secs(),
        );
        tracing::error!(
            machine_id = %mh_snapshot.host_snapshot.id,
            desired_version = %desired.version,
            ?boot_config_state,
            reason = %failure,
            "Restoring lockdown before parking BootConfiguring after its cleanup deadline",
        );
        return Ok(StateHandlerOutcome::transition(ready_boot_config_locking(
            desired,
            post_lock_verification_retry_count,
            Some(ReadyBootConfigPostLockAction::Convergence { failure }),
        )));
    }

    // `Prepare` can safely wait in place. Any state that may have opened
    // lockdown records why cleanup began so it returns to `Prepare` even if
    // the DPU `MachineNetworkStatusObservation` values become current before
    // the next controller iteration.
    if !dpu_network_status_current && ready_boot_config_may_have_opened_lockdown(&boot_config_state)
    {
        tracing::warn!(
            machine_id = %mh_snapshot.host_snapshot.id,
            desired_version = %desired.version,
            ?boot_config_state,
            "DPU network status is stale; restoring lockdown before returning to ReadyBootConfigState::Prepare",
        );
        return Ok(StateHandlerOutcome::transition(ready_boot_config_locking(
            desired,
            post_lock_verification_retry_count,
            Some(ReadyBootConfigPostLockAction::ReturnToPrepare),
        )));
    }

    // A terminal failure stays scoped to the captured desired generation.
    // A maintenance request can take control, while changing intent creates a
    // new version and restarts convergence.
    if matches!(boot_config_state, ReadyBootConfigState::Failed { .. }) {
        if let Some(outcome) = maintenance::maintenance_transition_if_requested(mh_snapshot) {
            return Ok(outcome);
        }
        return Ok(
            match ready_boot_config_superseded_state(desired.version, current_desired) {
                Some(next_state) => StateHandlerOutcome::transition(next_state),
                None => StateHandlerOutcome::do_nothing(),
            },
        );
    }

    if ready_boot_config_can_adopt_latest(&boot_config_state)
        && current_desired.as_ref().map(|current| current.version) != Some(desired.version)
    {
        return Ok(match current_desired {
            Some(current_desired) => {
                let next_state = if matches!(boot_config_state, ReadyBootConfigState::Prepare) {
                    ReadyBootConfigState::Prepare
                } else {
                    // Re-observe the replacement before any Redfish write.
                    ReadyBootConfigState::CheckHostConfig
                };
                StateHandlerOutcome::transition(ready_boot_configuring(
                    current_desired,
                    0,
                    next_state,
                ))
            }
            None if matches!(boot_config_state, ReadyBootConfigState::Prepare) => {
                StateHandlerOutcome::transition(ManagedHostState::Ready)
            }
            None => {
                // The target disappeared after lockdown may have opened.
                StateHandlerOutcome::transition(ready_boot_config_locking(
                    desired,
                    post_lock_verification_retry_count,
                    None,
                ))
            }
        });
    }

    if matches!(boot_config_state, ReadyBootConfigState::Prepare) {
        // Recover an unresponsive DPU before checking the `network_config_version` field on each
        // `MachineNetworkStatusObservation`. The host boot check below can then reuse this
        // decision from the same snapshot.
        if !are_dpus_up_trigger_reboot_if_needed(mh_snapshot, reachability_params, ctx).await {
            return Ok(StateHandlerOutcome::wait(
                "Waiting for DPUs to come up.".to_string(),
            ));
        }

        // Every DPU in the host topology must have a network status observation
        // whose version matches `host_snapshot.network_config.version` before
        // the controller starts host Redfish work.
        if !dpu_network_status_current {
            return Ok(StateHandlerOutcome::wait(
                "Waiting for every DPU in the host topology to apply the current host network configuration"
                    .to_string(),
            ));
        }
    }

    match boot_config_state {
        ReadyBootConfigState::Prepare => {
            if mh_snapshot.host_snapshot.bmc_vendor().is_supermicro() {
                // A locked Supermicro BMC can report stale boot order. Reboot
                // after disabling lockdown before performing the exact read.
                return Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                    desired,
                    post_lock_verification_retry_count,
                    ReadyBootConfigState::UnlockHost {
                        unlock_host_state: UnlockHostState::DisableLockdown,
                    },
                )));
            }

            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;

            let preflight_decision = match check_host_boot_config(
                redfish_client.as_ref(),
                mh_snapshot,
                reachability_params,
                HostBootConfigDpuFreshness::AlreadyValidated,
                Some(&captured_boot_interface),
                ctx,
            )
            .await?
            {
                HostBootConfigCheckOutcome::Wait(reason) => {
                    return Ok(StateHandlerOutcome::wait(reason));
                }
                HostBootConfigCheckOutcome::Ready(decision) => decision,
            };
            let preflight_complete = matches!(preflight_decision, HostBootConfigDecision::Complete);

            let next_state = if preflight_complete {
                // Avoid opening an ordinary host that is already correct.
                ReadyBootConfigState::LockHost {
                    post_lock_action: None,
                }
            } else {
                match redfish_client.lockdown_status().await {
                    Err(RedfishError::NotSupported(_)) => {
                        tracing::info!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            "BMC vendor does not support checking lockdown status during BootConfiguring",
                        );
                        ReadyBootConfigState::CheckHostConfig
                    }
                    Err(error) => {
                        tracing::warn!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            error = %error,
                            "Failed to fetch lockdown status during BootConfiguring",
                        );
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Failed to fetch lockdown status: {error}"
                        )));
                    }
                    Ok(lockdown_status) if !lockdown_status.is_fully_disabled() => {
                        ReadyBootConfigState::UnlockHost {
                            unlock_host_state: UnlockHostState::DisableLockdown,
                        }
                    }
                    Ok(_) => ReadyBootConfigState::CheckHostConfig,
                }
            };

            Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                desired,
                post_lock_verification_retry_count,
                next_state,
            )))
        }
        ReadyBootConfigState::UnlockHost { unlock_host_state } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;

            let next_state = match unlock_host_state {
                UnlockHostState::DisableLockdown => {
                    match redfish_client.lockdown_bmc(EnabledDisabled::Disabled).await {
                        Ok(()) => {}
                        Err(RedfishError::NotSupported(_)) => {
                            tracing::info!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                "BMC vendor does not support disabling lockdown during BootConfiguring",
                            );
                        }
                        Err(error) => return Err(redfish_error("lockdown_bmc", error)),
                    }

                    if mh_snapshot.host_snapshot.bmc_vendor().is_supermicro() {
                        ReadyBootConfigState::UnlockHost {
                            unlock_host_state: UnlockHostState::RebootHost,
                        }
                    } else {
                        ReadyBootConfigState::CheckHostConfig
                    }
                }
                UnlockHostState::RebootHost => {
                    host_power_control(
                        redfish_client.as_ref(),
                        &mh_snapshot.host_snapshot,
                        SystemPowerControl::ForceRestart,
                        ctx,
                    )
                    .await
                    .map_err(|error| {
                        StateHandlerError::GenericError(eyre!(
                            "failed to ForceRestart host after disabling BMC lockdown: {error}"
                        ))
                    })?;

                    ReadyBootConfigState::UnlockHost {
                        unlock_host_state: UnlockHostState::WaitForUefiBoot,
                    }
                }
                UnlockHostState::WaitForUefiBoot => {
                    let entered_at = mh_snapshot.host_snapshot.state.version.timestamp();
                    if wait(&entered_at, reachability_params.uefi_boot_wait) {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for UEFI boot to complete on {} after post-unlock reboot",
                            mh_snapshot.host_snapshot.id
                        )));
                    }
                    ReadyBootConfigState::CheckHostConfig
                }
            };

            Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                desired,
                post_lock_verification_retry_count,
                next_state,
            )))
        }
        ReadyBootConfigState::CheckHostConfig => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;

            let next_state = match check_host_boot_config(
                redfish_client.as_ref(),
                mh_snapshot,
                reachability_params,
                HostBootConfigDpuFreshness::CurrentHostState,
                Some(&captured_boot_interface),
                ctx,
            )
            .await?
            {
                HostBootConfigCheckOutcome::Wait(reason) => {
                    return Ok(StateHandlerOutcome::wait(reason));
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::ConfigureBios) => {
                    ReadyBootConfigState::ConfigureBios { retry_count: 0 }
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::SetBootOrder) => {
                    ReadyBootConfigState::SetBootOrder {
                        set_boot_order_info: initial_set_boot_order_info(),
                    }
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::Complete) => {
                    ReadyBootConfigState::LockHost {
                        post_lock_action: None,
                    }
                }
            };

            Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                desired,
                post_lock_verification_retry_count,
                next_state,
            )))
        }
        ReadyBootConfigState::ConfigureBios { retry_count } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;
            handle_ready_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                desired,
                post_lock_verification_retry_count,
                HostBootConfigStage::ConfigureBios { retry_count },
            )
            .await
        }
        ReadyBootConfigState::WaitingForBiosJob { bios_config_info } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;
            handle_ready_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                desired,
                post_lock_verification_retry_count,
                HostBootConfigStage::WaitingForBiosJob { bios_config_info },
            )
            .await
        }
        ReadyBootConfigState::PollingBiosSetup { retry_count } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;
            handle_ready_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                desired,
                post_lock_verification_retry_count,
                HostBootConfigStage::PollingBiosSetup { retry_count },
            )
            .await
        }
        ReadyBootConfigState::SetBootOrder {
            set_boot_order_info,
        } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;
            handle_ready_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                desired,
                post_lock_verification_retry_count,
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info,
                },
            )
            .await
        }
        ReadyBootConfigState::LockHost { post_lock_action } => {
            let lockdown_disabled = mh_snapshot.host_snapshot.host_profile.disable_lockdown;

            // A profile that deliberately leaves lockdown disabled has no
            // cleanup barrier. Deferred actions can therefore be applied
            // without requiring Redfish access. Successful convergence still
            // performs the final exact-target observation below.
            if lockdown_disabled && let Some(post_lock_action) = &post_lock_action {
                match post_lock_action {
                    ReadyBootConfigPostLockAction::ReturnToPrepare => {
                        return Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                            desired,
                            post_lock_verification_retry_count,
                            ReadyBootConfigState::Prepare,
                        )));
                    }
                    ReadyBootConfigPostLockAction::Machine {
                        machine_id,
                        details,
                    } => {
                        return Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                            details: details.clone(),
                            machine_id: *machine_id,
                            retry_count: 0,
                        }));
                    }
                    ReadyBootConfigPostLockAction::Convergence { failure } => {
                        let mut txn = ctx.services.db_pool.begin().await?;
                        let current_desired = db::machine_desired_boot_interface::lock(
                            txn.as_mut(),
                            &mh_snapshot.host_snapshot.id.try_into()?,
                        )
                        .await?;
                        let next_state =
                            ready_boot_config_superseded_state(desired.version, current_desired)
                                .unwrap_or_else(|| {
                                    ready_boot_configuring(
                                        desired,
                                        post_lock_verification_retry_count,
                                        ReadyBootConfigState::Failed {
                                            failure: failure.clone(),
                                        },
                                    )
                                });
                        return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
                    }
                }
            }

            if lockdown_disabled && !dpu_network_status_current {
                tracing::warn!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    desired_version = %desired.version,
                    "DPU network status is stale; returning to ReadyBootConfigState::Prepare because the host profile disables lockdown",
                );
                return Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                    desired,
                    post_lock_verification_retry_count,
                    ReadyBootConfigState::Prepare,
                )));
            }

            let redfish_client = match ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await
            {
                Ok(redfish_client) => redfish_client,
                Err(error) => {
                    tracing::warn!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        error = %error,
                        "Waiting for Redfish access before completing BootConfiguring cleanup",
                    );
                    return Ok(StateHandlerOutcome::wait(
                        "Waiting for host Redfish access before completing BootConfiguring cleanup"
                            .to_string(),
                    ));
                }
            };

            if lockdown_disabled {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "Skipping lockdown restoration because the host profile disables lockdown",
                );
            } else {
                let (lockdown_command_required, verify_after_command, require_supported_command) =
                    match redfish_client.lockdown_status().await {
                        Ok(lockdown_status) if lockdown_status.is_fully_enabled() => {
                            (false, false, false)
                        }
                        Ok(lockdown_status) => {
                            tracing::info!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                ?lockdown_status,
                                "Restoring lockdown during BootConfiguring",
                            );
                            (true, true, true)
                        }
                        Err(RedfishError::NotSupported(_)) => {
                            // The command may still be supported even when the
                            // vendor has no corresponding status read.
                            (true, false, false)
                        }
                        Err(error) => {
                            tracing::warn!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                error = %error,
                                "Could not read lockdown status during BootConfiguring cleanup; attempting restoration",
                            );
                            (true, true, true)
                        }
                    };

                let lockdown_command_supported = if lockdown_command_required {
                    match redfish_client.lockdown_bmc(EnabledDisabled::Enabled).await {
                        Ok(()) => true,
                        Err(RedfishError::NotSupported(_)) if require_supported_command => {
                            return Ok(StateHandlerOutcome::wait(
                                "BMC lockdown status is not verified and the BMC does not support restoring it"
                                    .to_string(),
                            ));
                        }
                        Err(RedfishError::NotSupported(_)) => {
                            tracing::info!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                "BMC vendor does not support restoring lockdown during BootConfiguring",
                            );
                            false
                        }
                        Err(error) => return Err(redfish_error("lockdown_bmc", error)),
                    }
                } else {
                    false
                };

                if lockdown_command_supported && verify_after_command {
                    match redfish_client.lockdown_status().await {
                        Ok(lockdown_status) if lockdown_status.is_fully_enabled() => {}
                        Ok(lockdown_status) => {
                            tracing::info!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                ?lockdown_status,
                                "Waiting for lockdown policy restoration during BootConfiguring",
                            );
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Waiting for lockdown to be fully enabled during BootConfiguring; current status: {lockdown_status:?}"
                            )));
                        }
                        Err(RedfishError::NotSupported(_)) => {
                            tracing::warn!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                "BMC stopped supporting lockdown verification after reporting a readable status",
                            );
                            return Ok(StateHandlerOutcome::wait(
                                "BMC lockdown restoration could not be verified".to_string(),
                            ));
                        }
                        Err(error) => {
                            tracing::warn!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                error = %error,
                                "Failed to verify lockdown during BootConfiguring",
                            );
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Failed to verify lockdown during BootConfiguring: {error}"
                            )));
                        }
                    }
                }
            }

            if let Some(post_lock_action) = post_lock_action {
                match post_lock_action {
                    ReadyBootConfigPostLockAction::ReturnToPrepare => {
                        return Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                            desired,
                            post_lock_verification_retry_count,
                            ReadyBootConfigState::Prepare,
                        )));
                    }
                    ReadyBootConfigPostLockAction::Machine {
                        machine_id,
                        details,
                    } => {
                        return Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                            details,
                            machine_id,
                            retry_count: 0,
                        }));
                    }
                    ReadyBootConfigPostLockAction::Convergence { failure } => {
                        let mut txn = ctx.services.db_pool.begin().await?;
                        let current_desired = db::machine_desired_boot_interface::lock(
                            txn.as_mut(),
                            &mh_snapshot.host_snapshot.id.try_into()?,
                        )
                        .await?;
                        let next_state =
                            ready_boot_config_superseded_state(desired.version, current_desired)
                                .unwrap_or_else(|| {
                                    ready_boot_configuring(
                                        desired,
                                        post_lock_verification_retry_count,
                                        ReadyBootConfigState::Failed { failure },
                                    )
                                });
                        return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
                    }
                }
            }

            if !dpu_network_status_current {
                tracing::warn!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    desired_version = %desired.version,
                    "DPU network status is stale; lockdown is restored and ReadyBootConfigState::Prepare will run next",
                );
                return Ok(StateHandlerOutcome::transition(ready_boot_configuring(
                    desired,
                    post_lock_verification_retry_count,
                    ReadyBootConfigState::Prepare,
                )));
            }

            let boot_config_verified =
                if mh_snapshot.host_snapshot.bmc_vendor().is_supermicro() && !lockdown_disabled {
                    // For a still-current desired generation, entry into LockHost
                    // durably records an exact read while Supermicro lockdown was
                    // disabled. Locked boot-order reads can be stale, so that
                    // checkpoint is stronger evidence than another Redfish read.
                    tracing::info!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        desired_version = %desired.version,
                        "Using the pre-lock Supermicro boot verification",
                    );
                    true
                } else {
                    // This state is restartable, so re-observe after lockdown is
                    // restored rather than trusting an earlier pre-crash read.
                    let inspection = inspect_host_boot_config(
                        redfish_client.as_ref(),
                        mh_snapshot,
                        &captured_boot_interface,
                    )
                    .await?;
                    matches!(
                        decide_host_boot_config(inspection),
                        HostBootConfigDecision::Complete
                    )
                };
            if !boot_config_verified {
                let mut txn = ctx.services.db_pool.begin().await?;
                let current_desired = db::machine_desired_boot_interface::lock(
                    txn.as_mut(),
                    &mh_snapshot.host_snapshot.id.try_into()?,
                )
                .await?;
                let next_state =
                    ready_boot_config_superseded_state(desired.version, current_desired)
                        .unwrap_or_else(|| {
                            if post_lock_verification_retry_count
                                < MAX_POST_LOCK_VERIFICATION_RETRIES
                            {
                                tracing::warn!(
                                    machine_id = %mh_snapshot.host_snapshot.id,
                                    desired_version = %desired.version,
                                    post_lock_verification_retry_count =
                                        post_lock_verification_retry_count + 1,
                                    "Boot configuration drifted after lockdown restoration; retrying reconciliation",
                                );
                            } else {
                                tracing::warn!(
                                    machine_id = %mh_snapshot.host_snapshot.id,
                                    desired_version = %desired.version,
                                    post_lock_verification_retry_count,
                                    "Boot configuration repeatedly drifted after lockdown restoration; parking reconciliation",
                                );
                            }
                            ready_boot_config_after_post_lock_drift(
                                desired,
                                post_lock_verification_retry_count,
                                mh_snapshot.host_snapshot.id,
                            )
                        });
                return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
            }

            let mut txn = ctx.services.db_pool.begin().await?;
            let verified = db::machine_desired_boot_interface::mark_verified(
                txn.as_mut(),
                &mh_snapshot.host_snapshot.id.try_into()?,
                desired.version,
                Utc::now(),
            )
            .await?;
            let next_state = if verified {
                ManagedHostState::Ready
            } else {
                match db::machine_desired_boot_interface::get(
                    txn.as_mut(),
                    &mh_snapshot.host_snapshot.id.try_into()?,
                )
                .await?
                {
                    Some(current_desired) => {
                        ready_boot_configuring(current_desired, 0, ReadyBootConfigState::Prepare)
                    }
                    None => ManagedHostState::Ready,
                }
            };

            Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
        }
        // Handled above so superseding intent can restart a parked failure.
        ReadyBootConfigState::Failed { .. } => Ok(StateHandlerOutcome::do_nothing()),
    }
}

/// Handles a shared boot-configuration stage during host initialization.
///
/// Called by `HostMachineStateHandler::handle_object_state` for its platform
/// configuration, vendor-job, polling, and boot-order states. This adapter maps
/// continued stages back into `MachineState`, proceeds to measurements on
/// completion, and reports terminal failure as a host-init BIOS setup failure.
async fn handle_host_init_boot_config_stage(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    stage: HostBootConfigStage,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match run_host_boot_config_stage(
        ctx,
        reachability_params,
        redfish_client,
        mh_snapshot,
        None,
        stage,
    )
    .await?
    {
        HostBootConfigOutcome::Continue(stage) => {
            let machine_state = match stage {
                HostBootConfigStage::ConfigureBios { retry_count } => {
                    MachineState::WaitingForPlatformConfiguration { retry_count }
                }
                HostBootConfigStage::WaitingForBiosJob { bios_config_info } => {
                    MachineState::WaitingForBiosJob { bios_config_info }
                }
                HostBootConfigStage::PollingBiosSetup { retry_count } => {
                    MachineState::PollingBiosSetup { retry_count }
                }
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info,
                } => MachineState::SetBootOrder {
                    set_boot_order_info: Some(set_boot_order_info),
                },
            };
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::HostInit { machine_state },
            ))
        }
        HostBootConfigOutcome::Complete => Ok(StateHandlerOutcome::transition(
            ManagedHostState::HostInit {
                machine_state: MachineState::Measuring {
                    measuring_state: MeasuringState::WaitingForMeasurements,
                },
            },
        )),
        HostBootConfigOutcome::Wait(reason) => Ok(StateHandlerOutcome::wait(reason)),
        HostBootConfigOutcome::Failed { failure } => {
            Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                details: FailureDetails {
                    cause: FailureCause::BiosSetupFailed { err: failure },
                    failed_at: Utc::now(),
                    source: FailureSource::StateMachineArea(StateMachineArea::HostInit),
                },
                machine_id: mh_snapshot.host_snapshot.id,
                retry_count: 0,
            }))
        }
    }
}

/// Finishes HostInit lockdown and records a fresh target-specific verification
/// in the same state-transition transaction when the final Redfish read agrees.
///
/// A failed or mismatched read does not block ingestion. The desired version
/// remains pending, keeps the host out of allocation, and is handled by the
/// Ready reconciliation path. If operator intent changes before the exact
/// version is committed, the replacement likewise remains pending.
async fn complete_host_init_lockdown(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    next_state: ManagedHostState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let outcome = StateHandlerOutcome::transition(next_state);
    let Some(desired) = mh_snapshot
        .host_snapshot
        .config
        .desired_boot_interface
        .as_ref()
    else {
        return Ok(outcome);
    };

    if mh_snapshot.host_snapshot.bmc_vendor().is_supermicro()
        && !mh_snapshot.host_snapshot.host_profile.disable_lockdown
    {
        // Supermicro boot-order reads can become stale after lockdown. HostInit
        // has no persisted pre-lock verification boundary, so leave this
        // generation pending for the Ready convergence flow.
        tracing::info!(
            machine_id = %mh_snapshot.host_snapshot.id,
            desired_version = %desired.version,
            "Deferring Supermicro boot verification until Ready convergence",
        );
        return Ok(outcome);
    }

    let redfish_client = match ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await
    {
        Ok(redfish_client) => redfish_client,
        Err(error) => {
            tracing::warn!(
                machine_id = %mh_snapshot.host_snapshot.id,
                desired_version = %desired.version,
                error = %error,
                "Could not create a Redfish client for HostInit boot verification; leaving it pending for Ready reconciliation",
            );
            return Ok(outcome);
        }
    };
    let boot_interface: BootInterfaceTarget = desired.value.clone().into();
    let inspection = match inspect_host_boot_config(
        redfish_client.as_ref(),
        mh_snapshot,
        &boot_interface,
    )
    .await
    {
        Ok(inspection) => inspection,
        Err(error) => {
            tracing::warn!(
                machine_id = %mh_snapshot.host_snapshot.id,
                desired_version = %desired.version,
                error = %error,
                "Could not verify boot configuration after HostInit lockdown; leaving it pending for Ready reconciliation",
            );
            return Ok(outcome);
        }
    };
    if !matches!(
        decide_host_boot_config(inspection),
        HostBootConfigDecision::Complete
    ) {
        tracing::warn!(
            machine_id = %mh_snapshot.host_snapshot.id,
            desired_version = %desired.version,
            "Boot configuration differs after HostInit lockdown; leaving it pending for Ready reconciliation",
        );
        return Ok(outcome);
    }

    let mut txn = ctx.services.db_pool.begin().await?;
    let verified = db::machine_desired_boot_interface::mark_verified(
        txn.as_mut(),
        &mh_snapshot.host_snapshot.id.try_into()?,
        desired.version,
        Utc::now(),
    )
    .await?;
    if !verified {
        tracing::info!(
            machine_id = %mh_snapshot.host_snapshot.id,
            desired_version = %desired.version,
            "Desired boot interface changed during HostInit verification; leaving it pending",
        );
    }
    Ok(outcome.with_txn(txn))
}

/// TODO: we need to handle the case where the job is deleted for some reason
/// Resolve the current site-wide UEFI target version (host_uefi or dpu_uefi)
/// from `sitewide_credential_rotation.target_version` so ingestion drives a
/// device to the version the fleet has moved to. Version 0 is the legacy
/// unversioned site-default path. Table-driven, mirroring BMC ingestion.
async fn current_site_uefi_target(
    db_pool: &sqlx::PgPool,
    credential_type: db::credential_rotation::CredentialRotationType,
) -> Result<u32, StateHandlerError> {
    let mut conn = db_pool.acquire().await.map_err(|e| {
        StateHandlerError::GenericError(eyre!(
            "acquire db connection for uefi rotation target: {e}"
        ))
    })?;
    let version = db::credential_rotation::current_target_version(&mut conn, credential_type)
        .await
        .map_err(|e| StateHandlerError::GenericError(eyre!("read uefi rotation target: {e}")))?
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre!(
                "no site-wide {credential_type:?} rotation target row exists; the backfill \
                 migration seeds one for every active credential type, so a missing row \
                 indicates a broken or unmigrated database"
            ))
        })?;
    // The column is constrained non-negative, so a failed conversion means a
    // corrupt value, not "no rotation" -- surface it rather than masking it.
    u32::try_from(version).map_err(|e| {
        StateHandlerError::GenericError(eyre!(
            "uefi rotation target_version {version} is out of range for u32: {e}"
        ))
    })
}

/// Resolve the site-wide UEFI credential to set on a device during ingestion:
/// the secret at the current `host_uefi`/`dpu_uefi` target version. The
/// low-level `redfish` `uefi_setup` no longer reads the credential store itself,
/// so we resolve the version (table-driven) and read the credential here.
///
/// Takes `db_pool` and `reader` rather than the whole `StateHandlerContext`
/// because the context is not `Send`/`Sync` and must not be held across an await
/// in a handler future (`&PgPool` and `&dyn CredentialReader` both are).
async fn resolve_site_uefi_credentials(
    db_pool: &sqlx::PgPool,
    reader: &dyn CredentialReader,
    credential_type: db::credential_rotation::CredentialRotationType,
) -> Result<Credentials, StateHandlerError> {
    let version = current_site_uefi_target(db_pool, credential_type).await?;
    let key = match credential_type {
        db::credential_rotation::CredentialRotationType::HostUefi => {
            CredentialKey::host_uefi_site_default(version)
        }
        db::credential_rotation::CredentialRotationType::DpuUefi => {
            CredentialKey::dpu_uefi_site_default(version)
        }
        other => {
            return Err(StateHandlerError::GenericError(eyre!(
                "resolve_site_uefi_credentials called with non-UEFI credential type {other:?}"
            )));
        }
    };
    reader
        .get_credentials(&key)
        .await
        .map_err(|e| {
            StateHandlerError::GenericError(eyre!(
                "read site UEFI credential {}: {e}",
                key.to_key_str()
            ))
        })?
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre!(
                "site UEFI credential {} is not set",
                key.to_key_str()
            ))
        })
}

async fn handle_host_uefi_setup(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &mut ManagedHostStateSnapshot,
    uefi_setup_info: UefiSetupInfo,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // A known BMC MAC is a hard precondition for driving UEFI setup: it keys the
    // host_uefi rotation bookkeeping recorded once the password job completes, so
    // refuse to touch the device if we cannot identify (and later track) it.
    let host_bmc_mac =
        state
            .host_snapshot
            .status
            .bmc_info
            .mac
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: state.host_snapshot.id.to_string(),
                missing: "bmc_mac",
            })?;

    match uefi_setup_info.uefi_setup_state.clone() {
        UefiSetupState::UnlockHost => {
            if state.host_snapshot.needs_bmc_unlock_for_uefi_setup() {
                let redfish_client = ctx
                    .services
                    .create_redfish_client_from_machine(&state.host_snapshot)
                    .await?;
                redfish_client
                    .lockdown_bmc(libredfish::EnabledDisabled::Disabled)
                    .await
                    .map_err(|e| redfish_error("lockdown", e))?;
            }

            Ok(StateHandlerOutcome::transition(
                ManagedHostState::HostInit {
                    machine_state: MachineState::UefiSetup {
                        uefi_setup_info: UefiSetupInfo {
                            uefi_password_jid: None,
                            uefi_setup_state: UefiSetupState::SetUefiPassword,
                        },
                    },
                },
            ))
        }
        UefiSetupState::SetUefiPassword => {
            let host_uefi_credentials = resolve_site_uefi_credentials(
                &ctx.services.db_pool,
                ctx.services.bmc_credential_ops.credential_reader(),
                db::credential_rotation::CredentialRotationType::HostUefi,
            )
            .await?;
            let host_bmc_access = ctx
                .services
                .bmc_access_info_for_machine(&state.host_snapshot)
                .await?;
            match ctx
                .services
                .bmc_credential_ops
                .uefi_setup(&host_bmc_access, false, host_uefi_credentials)
                .await
            {
                Ok(job_id) => Ok(StateHandlerOutcome::transition(
                    ManagedHostState::HostInit {
                        machine_state: MachineState::UefiSetup {
                            uefi_setup_info: UefiSetupInfo {
                                uefi_password_jid: job_id,
                                uefi_setup_state: UefiSetupState::WaitForPasswordJobScheduled,
                            },
                        },
                    },
                )),
                // Client creation failed (credential store, TCP, or the
                // vendor probe): return Err so the framework retries.
                // Falling through to the untested-vendor arm below would
                // permanently skip setting the BIOS password over a blip.
                Err(carbide_redfish::libredfish::CredentialOpError::ClientCreation(e)) => {
                    Err(e.into())
                }
                Err(e) => {
                    let msg = format!(
                        "failed to set the BIOS password on {} ({}): {}",
                        state.host_snapshot.id,
                        state.host_snapshot.bmc_vendor(),
                        e
                    );

                    // This feature has only been tested thoroughly on Dells, Lenovos, and Vikings.
                    if state.host_snapshot.bmc_vendor().is_dell()
                        || state.host_snapshot.bmc_vendor().is_lenovo()
                        || state.host_snapshot.bmc_vendor().is_nvidia()
                    {
                        return Err(StateHandlerError::GenericError(eyre::eyre!("{}", msg)));
                    }

                    // For all other vendors, allow ingestion even though we couldnt set the bios password
                    // An operator will have to set the bios password manually
                    tracing::info!(msg);

                    Ok(StateHandlerOutcome::transition(
                        ManagedHostState::HostInit {
                            machine_state: MachineState::WaitingForLockdown {
                                lockdown_info: LockdownInfo {
                                    state: LockdownState::SetLockdown,
                                    mode: LockdownMode::Enable,
                                },
                            },
                        },
                    ))
                }
            }
        }
        UefiSetupState::WaitForPasswordJobScheduled => {
            if let Some(job_id) = uefi_setup_info.uefi_password_jid.as_ref() {
                let redfish_client = ctx
                    .services
                    .create_redfish_client_from_machine(&state.host_snapshot)
                    .await?;
                let job_state = redfish_client
                    .get_job_state(job_id)
                    .await
                    .map_err(|e| redfish_error("get_job_state", e))?;

                if !matches!(job_state, libredfish::JobState::Scheduled) {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "waiting for job {:#?} to be scheduled; current state: {job_state:#?}",
                        job_id
                    )));
                }
            }

            Ok(StateHandlerOutcome::transition(
                ManagedHostState::HostInit {
                    machine_state: MachineState::UefiSetup {
                        uefi_setup_info: UefiSetupInfo {
                            uefi_password_jid: uefi_setup_info.uefi_password_jid.clone(),
                            uefi_setup_state: UefiSetupState::PowercycleHost,
                        },
                    },
                },
            ))
        }
        UefiSetupState::PowercycleHost => {
            handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart).await?;
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::HostInit {
                    machine_state: MachineState::UefiSetup {
                        uefi_setup_info: UefiSetupInfo {
                            uefi_password_jid: uefi_setup_info.uefi_password_jid.clone(),
                            uefi_setup_state: UefiSetupState::WaitForPasswordJobCompletion,
                        },
                    },
                },
            ))
        }
        UefiSetupState::WaitForPasswordJobCompletion => {
            if let Some(job_id) = uefi_setup_info.uefi_password_jid.as_ref() {
                let redfish_client = ctx
                    .services
                    .create_redfish_client_from_machine(&state.host_snapshot)
                    .await?;

                let job_state = redfish_client
                    .get_job_state(job_id)
                    .await
                    .map_err(|e| redfish_error("get_job_state", e))?;

                if !matches!(job_state, libredfish::JobState::Completed) {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "waiting for job {:#?} to complete; current state: {job_state:#?}",
                        job_id
                    )));
                }
            }

            let mut txn = ctx.services.db_pool.begin().await?;
            state.host_snapshot.bios_password_set_time = Some(chrono::offset::Utc::now());
            db::machine::update_bios_password_set_time(&state.host_snapshot.id, &mut txn)
                .await
                .map_err(|e| {
                    StateHandlerError::GenericError(eyre!(
                        "update_host_bios_password_set failed: {}",
                        e
                    ))
                })?;

            // The host's UEFI password is now the site-wide value: record it as
            // converged to the current host_uefi target so the rotation engine
            // tracks this host from ingestion onward (mirrors the backfill, which
            // keys host UEFI by the host BMC MAC). The MAC was validated as a
            // precondition at the top of this handler.
            db::credential_rotation::record_device_converged(
                &mut txn,
                host_bmc_mac,
                db::credential_rotation::CredentialRotationType::HostUefi,
            )
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!(
                    "record host_uefi credential rotation convergence failed: {}",
                    e
                ))
            })?;

            Ok(StateHandlerOutcome::transition(ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForLockdown {
                    lockdown_info: LockdownInfo {
                        state: LockdownState::SetLockdown,
                        mode: LockdownMode::Enable,
                    },
                },
            })
            .with_txn(txn))
        }
        // Deprecated: Kept for backwards compatibility with hosts that may be in this state.
        UefiSetupState::LockdownHost => Ok(StateHandlerOutcome::transition(
            ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForLockdown {
                    lockdown_info: LockdownInfo {
                        state: LockdownState::SetLockdown,
                        mode: LockdownMode::Enable,
                    },
                },
            },
        )),
    }
}

#[async_trait::async_trait]
impl StateHandler for HostMachineStateHandler {
    type State = ManagedHostStateSnapshot;
    type ControllerState = ManagedHostState;
    type ObjectId = MachineId;
    type ContextObjects = MachineStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        host_machine_id: &MachineId,
        mh_snapshot: &mut ManagedHostStateSnapshot,
        _controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        if let ManagedHostState::HostInit { machine_state } = &mh_snapshot.managed_state {
            match machine_state {
                MachineState::Init => Err(StateHandlerError::InvalidHostState(
                    *host_machine_id,
                    Box::new(mh_snapshot.managed_state.clone()),
                )),
                MachineState::EnableIpmiOverLan => {
                    let host_redfish_client = ctx
                        .services
                        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                        .await?;

                    if !host_redfish_client
                        .is_ipmi_over_lan_enabled()
                        .await
                        .map_err(|e| redfish_error("enable_ipmi_over_lan", e))?
                    {
                        tracing::info!(
                            machine_id = %host_machine_id,
                            "IPMI over LAN is currently disabled on this host--enabling IPMI over LAN");

                        host_redfish_client
                            .enable_ipmi_over_lan(libredfish::EnabledDisabled::Enabled)
                            .await
                            .map_err(|e| redfish_error("enable_ipmi_over_lan", e))?;
                    }

                    let next_state = ManagedHostState::HostInit {
                        machine_state: MachineState::WaitingForPlatformConfiguration {
                            retry_count: 0,
                        },
                    };

                    Ok(StateHandlerOutcome::transition(next_state))
                }
                MachineState::WaitingForPlatformConfiguration { retry_count } => {
                    tracing::info!(
                        machine_id = %host_machine_id,
                        "Starting UEFI / BMC setup");

                    let redfish_client = ctx
                        .services
                        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                        .await?;

                    match redfish_client.lockdown_status().await {
                        Err(libredfish::RedfishError::NotSupported(_)) => {
                            tracing::info!(
                                "BMC vendor does not support checking lockdown status for {host_machine_id}."
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Error fetching lockdown status for {host_machine_id} during machine_setup check: {e}"
                            );
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Failed to fetch lockdown status: {}",
                                e
                            )));
                        }
                        Ok(lockdown_status) if !lockdown_status.is_fully_disabled() => {
                            tracing::info!(
                                "Lockdown is enabled for {host_machine_id} during machine_setup, disabling now."
                            );
                            let next_state = ManagedHostState::HostInit {
                                machine_state: MachineState::WaitingForLockdown {
                                    lockdown_info: LockdownInfo {
                                        state: LockdownState::SetLockdown,
                                        mode: LockdownMode::Disable,
                                    },
                                },
                            };
                            return Ok(StateHandlerOutcome::transition(next_state));
                        }
                        Ok(_) => {
                            // Lockdown is disabled, proceed with machine_setup
                        }
                    }

                    // GB200 SBIOS can rebuild a stale UEFI HTTP option only when
                    // `hostusb0` is configured before the platform setup reboot.
                    gb200_host_interface::repair_gb200_host_interface_if_needed(
                        ctx,
                        redfish_client.as_ref(),
                        mh_snapshot,
                    )
                    .await?;

                    handle_host_init_boot_config_stage(
                        ctx,
                        mh_snapshot,
                        &self.host_handler_params.reachability_params,
                        redfish_client.as_ref(),
                        HostBootConfigStage::ConfigureBios {
                            retry_count: *retry_count,
                        },
                    )
                    .await
                }
                MachineState::WaitingForBiosJob { bios_config_info } => {
                    let redfish_client = ctx
                        .services
                        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                        .await?;
                    handle_host_init_boot_config_stage(
                        ctx,
                        mh_snapshot,
                        &self.host_handler_params.reachability_params,
                        redfish_client.as_ref(),
                        HostBootConfigStage::WaitingForBiosJob {
                            bios_config_info: bios_config_info.clone(),
                        },
                    )
                    .await
                }
                MachineState::PollingBiosSetup { retry_count } => {
                    let redfish_client = ctx
                        .services
                        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                        .await?;
                    handle_host_init_boot_config_stage(
                        ctx,
                        mh_snapshot,
                        &self.host_handler_params.reachability_params,
                        redfish_client.as_ref(),
                        HostBootConfigStage::PollingBiosSetup {
                            retry_count: *retry_count,
                        },
                    )
                    .await
                }
                MachineState::SetBootOrder {
                    set_boot_order_info,
                } => {
                    let Some(set_boot_order_info) = set_boot_order_info.clone() else {
                        return Ok(StateHandlerOutcome::transition(
                            ManagedHostState::HostInit {
                                machine_state: MachineState::SetBootOrder {
                                    set_boot_order_info: Some(initial_set_boot_order_info()),
                                },
                            },
                        ));
                    };
                    let redfish_client = ctx
                        .services
                        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                        .await?;
                    handle_host_init_boot_config_stage(
                        ctx,
                        mh_snapshot,
                        &self.host_handler_params.reachability_params,
                        redfish_client.as_ref(),
                        HostBootConfigStage::SetBootOrder {
                            set_boot_order_info,
                        },
                    )
                    .await
                }
                MachineState::Measuring { measuring_state } => {
                    if !self.host_handler_params.attestation_enabled {
                        return Ok(StateHandlerOutcome::transition(
                            ManagedHostState::HostInit {
                                machine_state: MachineState::SpdmMeasuring {
                                    spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                                },
                            },
                        ));
                    }
                    match handle_measuring_state(
                        measuring_state,
                        &mh_snapshot.host_snapshot.id,
                        &mut ctx.services.db_reader,
                        self.host_handler_params.attestation_enabled,
                        mh_snapshot.host_snapshot.state.version.timestamp(),
                        self.host_handler_params
                            .reachability_params
                            .waiting_for_measurements_timeout,
                    )
                    .await
                    {
                        Ok(measuring_outcome) => {
                            map_host_init_measuring_outcome_to_state_handler_outcome(
                                &measuring_outcome,
                                measuring_state,
                            )
                        }
                        Err(StateHandlerError::MissingData {
                            object_id: _,
                            missing: "ek_cert_verification_status",
                        }) => {
                            Ok(StateHandlerOutcome::wait(
                                "Waiting for Scout to start and send registration info (in discover_machine)".to_string(),
                            ))
                        }
                        Err(e) => Err(e),
                    }
                }
                MachineState::SpdmMeasuring {
                    spdm_measuring_state,
                } => {
                    let next_skip_state = ManagedHostState::HostInit {
                        machine_state: MachineState::WaitingForDiscovery,
                    };
                    if !ctx.services.site_config.spdm_enabled {
                        return Ok(StateHandlerOutcome::transition(next_skip_state));
                    }
                    match spdm_measuring_state {
                        SpdmMeasuringState::TriggerMeasurements => {
                            handle_spdm_trigger_state(
                                ctx.services,
                                mh_snapshot,
                                host_machine_id,
                                ManagedHostState::HostInit {
                                    machine_state: MachineState::SpdmMeasuring {
                                        spdm_measuring_state: SpdmMeasuringState::PollResult,
                                    },
                                },
                                next_skip_state,
                            )
                            .await
                        }
                        SpdmMeasuringState::PollResult => {
                            handle_spdm_poll_state(
                                &ctx.services.db_pool,
                                host_machine_id,
                                FailureSource::StateMachineArea(StateMachineArea::HostInit),
                                next_skip_state,
                            )
                            .await
                        }
                    }
                }
                MachineState::WaitingForDiscovery => {
                    let discovery_pending = !discovered_after_state_transition(
                        mh_snapshot.host_snapshot.state.version,
                        mh_snapshot.host_snapshot.status.last_discovery_time,
                    );
                    if discovery_pending
                        && gb200_host_interface::gb200_host_interface_address_is_missing(
                            ctx,
                            mh_snapshot,
                        )
                        .await?
                    {
                        // Persist the rewind first. The next iteration repairs
                        // the BMC from the state that already owns platform setup.
                        tracing::warn!(
                            machine_id = %host_machine_id,
                            "GB200 BMC hostusb0 static IPv4 address is missing; returning to platform configuration before another discovery boot"
                        );
                        return Ok(StateHandlerOutcome::transition(
                            ManagedHostState::HostInit {
                                machine_state: MachineState::WaitingForPlatformConfiguration {
                                    retry_count: 0,
                                },
                            },
                        ));
                    }

                    // Storage cleanup is a destructive disk wipe (NVMe/HDD) that scout runs after a
                    // reset, so only a real, discovered host enters it. A predicted host waits for
                    // discovery to promote it; the promoted host then does the cleanup.
                    // (machine_scout.rs mirrors this on the scout side.)
                    if mh_snapshot.host_snapshot.status.last_cleanup_time.is_none()
                        && host_machine_id.machine_type().is_host()
                    {
                        return Ok(StateHandlerOutcome::transition(waiting_for_cleanup_state(
                            CleanupState::Init,
                            CleanupContext::InitialDiscovery,
                        )));
                    }

                    if discovery_pending {
                        tracing::trace!(
                            machine_id = %host_machine_id,
                            host_last_seen = ?mh_snapshot.host_snapshot.status.last_discovery_time,
                            minimum_discovery_time = %mh_snapshot.host_snapshot.state.version.timestamp(),
                            "Waiting for forge-scout to report host online; host discovery predates the current state transition"
                        );
                        let status = trigger_reboot_if_needed(
                            &mh_snapshot.host_snapshot,
                            mh_snapshot,
                            None,
                            &self.host_handler_params.reachability_params,
                            ctx,
                        )
                        .await?;
                        return Ok(StateHandlerOutcome::wait(status.status));
                    }

                    Ok(StateHandlerOutcome::transition(
                        ManagedHostState::HostInit {
                            machine_state: MachineState::UefiSetup {
                                uefi_setup_info: UefiSetupInfo {
                                    uefi_password_jid: None,
                                    uefi_setup_state: UefiSetupState::SetUefiPassword,
                                },
                            },
                        },
                    ))
                }
                MachineState::UefiSetup { uefi_setup_info } => {
                    Ok(handle_host_uefi_setup(ctx, mh_snapshot, uefi_setup_info.clone()).await?)
                }
                MachineState::WaitingForLockdown { lockdown_info } => {
                    match &lockdown_info.state {
                        LockdownState::SetLockdown => {
                            if lockdown_info.mode == LockdownMode::Enable
                                && mh_snapshot.host_snapshot.host_profile.disable_lockdown
                            {
                                tracing::info!(
                                    machine_id = %host_machine_id,
                                    "Lockdown disabled per expected-machine config, skipping lockdown enable"
                                );
                                return complete_host_init_lockdown(
                                    ctx,
                                    mh_snapshot,
                                    ManagedHostState::BomValidating {
                                        bom_validating_state: BomValidating::MatchingSku(
                                            BomValidatingContext {
                                                machine_validation_context: Some(
                                                    MachineValidationContext::Discovery,
                                                ),
                                                ..BomValidatingContext::default()
                                            },
                                        ),
                                    },
                                )
                                .await;
                            }

                            tracing::info!(
                                machine_id = %host_machine_id,
                                mode = ?lockdown_info.mode,
                                "Setting lockdown and issuing reboot"
                            );

                            let redfish_client = ctx
                                .services
                                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                                .await?;

                            let action = match lockdown_info.mode {
                                LockdownMode::Enable => libredfish::EnabledDisabled::Enabled,
                                LockdownMode::Disable => libredfish::EnabledDisabled::Disabled,
                            };

                            redfish_client
                                .lockdown(action)
                                .await
                                .map_err(|e| redfish_error("lockdown", e))?;

                            handler_host_power_control(
                                mh_snapshot,
                                ctx,
                                SystemPowerControl::ForceRestart,
                            )
                            .await?;

                            Ok(StateHandlerOutcome::transition(
                                ManagedHostState::HostInit {
                                    machine_state: MachineState::WaitingForLockdown {
                                        lockdown_info: LockdownInfo {
                                            state: LockdownState::TimeWaitForDPUDown,
                                            mode: lockdown_info.mode.clone(),
                                        },
                                    },
                                },
                            ))
                        }
                        LockdownState::TimeWaitForDPUDown => {
                            if !mh_snapshot.has_managed_dpus() {
                                // There is no DPU power cycle to observe for
                                // `Nic`/`Ignore` hosts. Disable still needs the
                                // shared status poll to reach platform
                                // configuration. Preserve the established
                                // Enable path, which does not require a
                                // separately readable lockdown status.
                                if lockdown_info.mode == LockdownMode::Enable {
                                    return complete_host_init_lockdown(
                                        ctx,
                                        mh_snapshot,
                                        ManagedHostState::BomValidating {
                                            bom_validating_state: BomValidating::MatchingSku(
                                                BomValidatingContext {
                                                    machine_validation_context: Some(
                                                        MachineValidationContext::Discovery,
                                                    ),
                                                    ..BomValidatingContext::default()
                                                },
                                            ),
                                        },
                                    )
                                    .await;
                                }
                                return Ok(StateHandlerOutcome::transition(
                                    ManagedHostState::HostInit {
                                        machine_state: MachineState::WaitingForLockdown {
                                            lockdown_info: LockdownInfo {
                                                state: LockdownState::PollingLockdownStatus,
                                                mode: lockdown_info.mode.clone(),
                                            },
                                        },
                                    },
                                ));
                            }
                            // Lets wait for some time before checking if DPU is up or not.
                            // Waiting is needed because DPU takes some time to go down. If we check DPU
                            // reachability before it goes down, it will give us wrong result.
                            if wait(
                                &mh_snapshot.host_snapshot.state.version.timestamp(),
                                self.host_handler_params.reachability_params.dpu_wait_time,
                            ) {
                                Ok(StateHandlerOutcome::wait(format!(
                                    "Forced wait of {} for DPU to power down",
                                    self.host_handler_params.reachability_params.dpu_wait_time
                                )))
                            } else {
                                let next_state = ManagedHostState::HostInit {
                                    machine_state: MachineState::WaitingForLockdown {
                                        lockdown_info: LockdownInfo {
                                            state: LockdownState::WaitForDPUUp,
                                            mode: lockdown_info.mode.clone(),
                                        },
                                    },
                                };
                                Ok(StateHandlerOutcome::transition(next_state))
                            }
                        }
                        LockdownState::WaitForDPUUp => {
                            // Has forge-dpu-agent reported state? That means DPU is up.
                            if are_dpus_up_trigger_reboot_if_needed(
                                mh_snapshot,
                                &self.host_handler_params.reachability_params,
                                ctx,
                            )
                            .await
                            {
                                // reboot host
                                // When forge changes BIOS params (for lockdown enable/disable both), host does a power cycle.
                                // During power cycle, DPU also reboots. Now DPU and Host are coming up together. Since DPU is not ready yet,
                                // it does not forward DHCP discover from host and host goes into failure mode and stops sending further
                                // DHCP Discover. A second reboot starts DHCP cycle again when DPU is already up.

                                handler_host_power_control(
                                    mh_snapshot,
                                    ctx,
                                    SystemPowerControl::ForceRestart,
                                )
                                .await?;

                                let next_state = ManagedHostState::HostInit {
                                    machine_state: MachineState::WaitingForLockdown {
                                        lockdown_info: LockdownInfo {
                                            state: LockdownState::PollingLockdownStatus,
                                            mode: lockdown_info.mode.clone(),
                                        },
                                    },
                                };
                                Ok(StateHandlerOutcome::transition(next_state))
                            } else {
                                // The DPU can only come up while the host is
                                // powered on.
                                if is_host_powered_off(mh_snapshot, ctx).await? {
                                    tracing::error!(
                                        machine_id = %mh_snapshot.host_snapshot.id,
                                        "Host is powered off while waiting for DPU to report UP."
                                    );

                                    // TODO: power the host back on as a workaround. Lets wait and see if we can root cause why a host was powere off here.
                                    return Err(StateHandlerError::GenericError(eyre!(
                                        "host {} is powered off while waiting for DPU to report UP",
                                        mh_snapshot.host_snapshot.id
                                    )));
                                }
                                Ok(StateHandlerOutcome::wait("Waiting for DPU to report UP. This requires forge-dpu-agent to call the RecordDpuNetworkStatus API".to_string()))
                            }
                        }
                        LockdownState::PollingLockdownStatus => {
                            let next_state = if LockdownMode::Enable == lockdown_info.mode {
                                ManagedHostState::BomValidating {
                                    bom_validating_state: BomValidating::MatchingSku(
                                        BomValidatingContext {
                                            machine_validation_context: Some(
                                                MachineValidationContext::Discovery,
                                            ),
                                            ..BomValidatingContext::default()
                                        },
                                    ),
                                }
                            } else {
                                ManagedHostState::HostInit {
                                    machine_state: MachineState::WaitingForPlatformConfiguration {
                                        retry_count: 0,
                                    },
                                }
                            };

                            let redfish_client = ctx
                                .services
                                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                                .await?;

                            match redfish_client.lockdown_status().await {
                                Ok(lockdown_status) => {
                                    let expected_state = match lockdown_info.mode {
                                        LockdownMode::Enable => lockdown_status.is_fully_enabled(),
                                        LockdownMode::Disable => {
                                            lockdown_status.is_fully_disabled()
                                        }
                                    };

                                    if expected_state {
                                        tracing::info!(
                                            machine_id = %mh_snapshot.host_snapshot.id,
                                            mode = ?lockdown_info.mode,
                                            "Lockdown status verified successfully"
                                        );
                                        if lockdown_info.mode == LockdownMode::Enable {
                                            complete_host_init_lockdown(
                                                ctx,
                                                mh_snapshot,
                                                next_state,
                                            )
                                            .await
                                        } else {
                                            Ok(StateHandlerOutcome::transition(next_state))
                                        }
                                    } else {
                                        Ok(StateHandlerOutcome::wait(format!(
                                            "Polling lockdown status, waiting for {:?} to be applied. Current status: {:?}",
                                            lockdown_info.mode, lockdown_status
                                        )))
                                    }
                                }
                                Err(libredfish::RedfishError::NotSupported(_)) => {
                                    tracing::info!(
                                        machine_id = %host_machine_id,
                                        "BMC vendor does not support checking lockdown status."
                                    );
                                    if lockdown_info.mode == LockdownMode::Enable {
                                        complete_host_init_lockdown(ctx, mh_snapshot, next_state)
                                            .await
                                    } else {
                                        Ok(StateHandlerOutcome::transition(next_state))
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        machine_id = %mh_snapshot.host_snapshot.id,
                                        error = %e,
                                        "Failed to check lockdown status, will retry"
                                    );
                                    Ok(StateHandlerOutcome::wait(format!(
                                        "Failed to check lockdown status: {}. Will retry.",
                                        e
                                    )))
                                }
                            }
                        }
                    }
                }
                MachineState::Discovered {
                    skip_reboot_wait: skip_reboot,
                } => {
                    // Check if machine is rebooted. If yes, move to Ready state
                    // or Measuring state, depending on if machine attestation
                    // is enabled or not.
                    if rebooted(&mh_snapshot.host_snapshot) || *skip_reboot {
                        let next_state =
                            pending_ready_boot_config_state(&mh_snapshot.host_snapshot)
                                .unwrap_or(ManagedHostState::Ready);
                        Ok(StateHandlerOutcome::transition(next_state))
                    } else {
                        let status = trigger_reboot_if_needed(
                            &mh_snapshot.host_snapshot,
                            mh_snapshot,
                            None,
                            &self.host_handler_params.reachability_params,
                            ctx,
                        )
                        .await?;
                        Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for scout to call RebootCompleted grpc. {}",
                            status.status
                        )))
                    }
                }
            }
        } else {
            Err(StateHandlerError::InvalidHostState(
                *host_machine_id,
                Box::new(mh_snapshot.managed_state.clone()),
            ))
        }
    }
}

/// A `StateHandler` implementation for instances
#[derive(Debug, Clone)]
pub struct InstanceStateHandler {
    reachability_params: ReachabilityParams,
    common_pools: Option<Arc<CommonPools>>,
    host_upgrade: Arc<HostUpgradeState>,
    hardware_models: FirmwareConfig,
    enable_secure_boot: bool,
    dpf_sdk: Option<Arc<dyn DpfOperations>>,
}

impl InstanceStateHandler {
    #[allow(clippy::too_many_arguments)]
    fn new(
        reachability_params: ReachabilityParams,
        common_pools: Option<Arc<CommonPools>>,
        host_upgrade: Arc<HostUpgradeState>,
        hardware_models: FirmwareConfig,
        enable_secure_boot: bool,
        dpf_sdk: Option<Arc<dyn DpfOperations>>,
    ) -> Self {
        InstanceStateHandler {
            reachability_params,
            common_pools,
            host_upgrade,
            hardware_models,
            enable_secure_boot,
            dpf_sdk,
        }
    }
}

#[async_trait::async_trait]
impl StateHandler for InstanceStateHandler {
    type State = ManagedHostStateSnapshot;
    type ControllerState = ManagedHostState;
    type ObjectId = MachineId;
    type ContextObjects = MachineStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        host_machine_id: &MachineId,
        mh_snapshot: &mut ManagedHostStateSnapshot,
        _controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let Some(ref instance) = mh_snapshot.instance else {
            return Err(StateHandlerError::GenericError(eyre!(
                "instance is empty at this point. cleanup is needed for host: {}",
                host_machine_id
            )));
        };

        if let ManagedHostState::Assigned { instance_state } = &mh_snapshot.managed_state {
            match instance_state {
                InstanceState::Init => {
                    // we should not be here. This state to be used if state machine has not
                    // picked instance creation and user asked for status.
                    Err(StateHandlerError::InvalidHostState(
                        *host_machine_id,
                        Box::new(mh_snapshot.managed_state.clone()),
                    ))
                }
                InstanceState::WaitingForNetworkSegmentToBeReady => {
                    let next_state = ManagedHostState::Assigned {
                        instance_state: InstanceState::WaitingForNetworkConfig,
                    };
                    let network_segment_ids_with_vpc = instance
                        .config
                        .network
                        .interfaces
                        .iter()
                        .filter_map(InstanceInterfaceConfig::generated_network_segment_id)
                        .collect_vec();

                    // No generated VPC-prefix segment needs readiness tracking.
                    if network_segment_ids_with_vpc.is_empty() {
                        return Ok(StateHandlerOutcome::transition(next_state));
                    }

                    let network_segments_are_ready =
                        db::network_segment::are_network_segments_ready(
                            &mut ctx.services.db_reader,
                            &network_segment_ids_with_vpc,
                        )
                        .await?;
                    if !network_segments_are_ready {
                        return Ok(StateHandlerOutcome::wait(
                            "Waiting for all segments to come in ready state.".to_string(),
                        ));
                    }
                    Ok(StateHandlerOutcome::transition(next_state))
                }
                InstanceState::WaitingForNetworkConfig => {
                    // It should be first state to process here.
                    // Wait for instance network config to be applied
                    // Reboot host and moved to Ready.

                    // A released instance no longer needs its tenant network config to converge.
                    // Waiting here can deadlock deletion when the DPU agents are unavailable (or
                    // when a simulator restarts) because the observations we are waiting for will
                    // never arrive. Continue through the normal deletion reboot path instead.
                    if instance.deleted.is_some() {
                        return Ok(StateHandlerOutcome::transition(
                            ManagedHostState::Assigned {
                                instance_state: InstanceState::WaitingForRebootToReady,
                            },
                        ));
                    }

                    // Check DPU network config has been applied
                    if !mh_snapshot.managed_host_network_config_version_synced() {
                        return Ok(StateHandlerOutcome::wait(
                                    "Waiting for DPU agent(s) to apply network config and report healthy network"
                                        .to_string()
                                ));
                    }

                    // Check each DPA interface to see if it has acted on updating the network config.
                    // This involves the DPA State Machine sending SetVNI commands to the NICs, and getting
                    // an ACK. If any of the interfaces has not yet heard back the ACk, we will continue to
                    // be in the current state.
                    if ctx.services.site_config.ewethers_enabled {
                        for dpa_interface in &mh_snapshot.dpa_interface_snapshots {
                            if !dpa_interface.managed_host_network_config_version_synced(
                                &mh_snapshot.instance,
                                &mh_snapshot.host_snapshot.status.spx_status_observation,
                            ) {
                                return Ok(StateHandlerOutcome::wait(
                                            "Waiting for DPA agent(s) to apply network config and report healthy network"
                                                .to_string()
                                        ));
                            }
                        }
                    }

                    let next_state = ManagedHostState::Assigned {
                        instance_state: InstanceState::WaitingForRebootToReady,
                    };

                    // Check instance network config has been applied
                    match check_instance_network_synced_and_dpu_healthy(instance, mh_snapshot)? {
                        InstanceNetworkSyncStatus::InstanceNetworkObservationNotAvailable(
                            missing_dpus,
                        ) => {
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Waiting for DPU agents to apply initial network config for DPUs: {}",
                                missing_dpus.iter().map(|dpu| dpu.to_string()).join(", ")
                            )));
                        }
                        InstanceNetworkSyncStatus::InstanceNetworkSynced => {}
                        InstanceNetworkSyncStatus::ZeroDpuNoObservationNeeded => {
                            // We don't need the DPU observation - but we still want to check
                            // whether NVLink and IB configs are applied
                        }
                        InstanceNetworkSyncStatus::InstanceNetworkNotSynced(outdated_dpus) => {
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Waiting for DPU agent to apply most recent network config for DPUs: {}",
                                outdated_dpus.iter().map(|dpu| dpu.to_string()).join(", ")
                            )));
                        }
                    };

                    if primary_dpu_has_pxe_blocking_bgp_alert(mh_snapshot) {
                        return Ok(StateHandlerOutcome::wait(
                            PRIMARY_DPU_BGP_WAIT_REASON.to_string(),
                        ));
                    }

                    // Check whether the IB config is synced
                    if let Err(not_synced_reason) = ib_config_synced(
                        mh_snapshot
                            .host_snapshot
                            .status
                            .infiniband_status_observation
                            .as_ref(),
                        Some(&instance.config.infiniband),
                        true,
                    ) {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for IB config to be applied: {}",
                            not_synced_reason
                        )));
                    }

                    // Check if the nvlink config has been applied
                    if let Err(not_synced_reason) = nvlink_config_synced(
                        mh_snapshot
                            .host_snapshot
                            .status
                            .nvlink_status_observation
                            .as_ref(),
                        Some(&instance.config.nvlink),
                    ) {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for NvLink config to be applied: {}",
                            not_synced_reason.0
                        )));
                    }
                    Ok(StateHandlerOutcome::transition(next_state))
                }
                InstanceState::WaitingForStorageConfig => {
                    // This state used to do something but doesn't any more, we can delete
                    // InstanceState::WaitingForStorageConfig once we're sure no places have the
                    // state persisted.
                    let next_state = ManagedHostState::Assigned {
                        instance_state: InstanceState::WaitingForExtensionServicesConfig,
                    };
                    Ok(StateHandlerOutcome::transition(next_state))
                }
                InstanceState::WaitingForExtensionServicesConfig => {
                    // Extension services run on DPUs. A zero-DPU host has no
                    // DPUs to run them on, so there is nothing to wait for;
                    // skip straight to the next state.
                    if !mh_snapshot.has_managed_dpus() {
                        let next_state = ManagedHostState::Assigned {
                            instance_state: InstanceState::WaitingForRebootToReady,
                        };
                        return Ok(StateHandlerOutcome::transition(next_state));
                    }

                    // If no extension services are configured, skip the wait and proceed
                    if instance
                        .config
                        .extension_services
                        .service_configs
                        .is_empty()
                    {
                        let next_state = ManagedHostState::Assigned {
                            instance_state: InstanceState::WaitingForRebootToReady,
                        };
                        return Ok(StateHandlerOutcome::transition(next_state));
                    }

                    let mut extension_services_status = get_extension_services_status(
                        mh_snapshot,
                        instance,
                        &ctx.services.db_pool,
                        self.dpf_sdk.as_deref(),
                    )
                    .await?;
                    let extension_services_status = &mut extension_services_status;
                    let txn = if extension_services_status.configs_synced == SyncState::Synced
                        && !extension_services_status
                            .get_terminated_service_keys()
                            .is_empty()
                    {
                        let mut txn = ctx.services.db_pool.begin().await?;
                        cleanup_terminated_extension_services(
                            instance,
                            extension_services_status,
                            txn.as_mut(),
                        )
                        .await?;

                        Some(txn)
                    } else {
                        None
                    };
                    let outcome = match extension_service::compute_extension_services_readiness(extension_services_status) {
                                ExtensionServicesReadiness::Ready => {
                                    let next_state = ManagedHostState::Assigned {
                                        instance_state: InstanceState::WaitingForRebootToReady,
                                    };
                                    StateHandlerOutcome::transition(next_state)
                                }
                                ExtensionServicesReadiness::ConfigsPending => {
                                    StateHandlerOutcome::wait(
                                        "Waiting for extension services config to be applied on all DPUs.".to_string(),
                                    )
                                }
                                ExtensionServicesReadiness::NotFullyRunning => {
                                    StateHandlerOutcome::wait(
                                        "Waiting for all active extension services to be running on all DPUs.".to_string(),
                                    )
                                }
                                ExtensionServicesReadiness::SomeTerminating => {
                                    StateHandlerOutcome::wait(
                                        "Waiting for all terminating extension services to be fully terminated across all DPUs."
                                            .to_string(),
                                    )
                                }
                            };
                    Ok(match txn {
                        Some(txn) => outcome.with_txn(txn),
                        None => outcome,
                    })
                }
                InstanceState::WaitingForRebootToReady => {
                    // Deletion and custom iPXE requests intentionally bypass these readiness
                    // checks. During normal provisioning, recheck aggregate health for every host.
                    // Hosts with managed DPUs also recheck the primary DPU p0 BGP alert. This
                    // closes the gap between the network configuration check and the restart.
                    if instance.deleted.is_none()
                        && !instance.custom_pxe_reboot_requested
                        && let Some(reason) = provisioning_pxe_reboot_wait_reason(mh_snapshot)?
                    {
                        return Ok(StateHandlerOutcome::wait(reason.to_string()));
                    }

                    // If custom_pxe_reboot_requested is set, this reboot was triggered by
                    // the tenant requested a boot with custom iPXE. Clear the request flag.
                    // The use_custom_pxe_on_boot flag was already set by the API handler.
                    if instance.custom_pxe_reboot_requested {
                        ctx.pending_db_writes
                            .push(MachineWriteOp::SetCustomPxeRebootRequested {
                                machine_id: mh_snapshot.host_snapshot.id,
                                requested: false,
                            });
                    }

                    let host = &mh_snapshot.host_snapshot;
                    if let Some(restart) =
                        host.status
                            .last_reboot_requested
                            .as_ref()
                            .filter(|restart| {
                                restart.mode == MachineLastRebootRequestedMode::Reboot
                                    && restart.time > host.state.version.timestamp()
                            })
                    {
                        let restart_completed = host
                            .status
                            .last_reboot_time
                            .is_some_and(|completed| completed > restart.time);
                        if restart_completed || restart.restart_verified == Some(true) {
                            return Ok(StateHandlerOutcome::transition(
                                ManagedHostState::Assigned {
                                    instance_state: InstanceState::Ready,
                                },
                            ));
                        }

                        return Ok(StateHandlerOutcome::wait(
                            "Waiting for host restart completion or verification.".to_string(),
                        ));
                    }

                    handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart)
                        .await?;
                    Ok(StateHandlerOutcome::wait(
                        "Waiting for host restart completion or verification.".to_string(),
                    ))
                }
                InstanceState::Ready => {
                    // Machine is up after reboot. Hurray. Instance is up.

                    // Wait for user's approval. Once user approves for dpu
                    // reprovision/update firmware, trigger it.
                    let is_auto_approved = self.host_upgrade.is_auto_approved();

                    // We will give first priority to network config update.
                    // This is the easiest way to stop resource leakage.
                    if instance.update_network_config_request.is_some() {
                        // Tenant has requested network config update.
                        let next_state = ManagedHostState::Assigned {
                            instance_state: InstanceState::NetworkConfigUpdate {
                                network_config_update_state:
                                    NetworkConfigUpdateState::WaitingForNetworkSegmentToBeReady,
                            },
                        };
                        return Ok(StateHandlerOutcome::transition(next_state));
                    }

                    let reprov_can_be_started =
                        if dpu_reprovisioning_needed(&mh_snapshot.dpu_snapshots) {
                            // Usually all DPUs are updated with user_approval_received field as true
                            // if `invoke_instance_power` is called.
                            // TODO: multidpu: Move this field to `instances` table and unset on
                            // reprovision is completed.
                            mh_snapshot
                                .dpu_snapshots
                                .iter()
                                .filter(|x| x.reprovision_requested.is_some())
                                .all(|x| {
                                    x.reprovision_requested
                                        .as_ref()
                                        .map(|x| x.user_approval_received || is_auto_approved)
                                        .unwrap_or_default()
                                })
                        } else {
                            false
                        };
                    let host_firmware_requested = if let Some(request) =
                        &mh_snapshot.host_snapshot.host_reprovision_requested
                    {
                        request.user_approval_received || is_auto_approved
                    } else {
                        false
                    };

                    // Check if the instance needs to PXE boot. The
                    // custom_pxe_reboot_requested flag is set by the API when the tenant calls
                    // InvokeInstancePower with boot_with_custom_ipxe=true.
                    // This triggers the HostPlatformConfiguration flow to verify BIOS boot order
                    // before rebooting. The WaitingForRebootToReady handler will clear this flag
                    // and set use_custom_pxe_on_boot, which the iPXE handler uses to serve the
                    // tenant's script.
                    let boot_with_custom_ipxe = instance.custom_pxe_reboot_requested;
                    // Run cleanup here so fully terminated extension services are
                    // removed from persisted instance config.
                    let mut txn_opt = None;
                    if !instance
                        .config
                        .extension_services
                        .service_configs
                        .is_empty()
                    {
                        match get_extension_services_status(
                            mh_snapshot,
                            instance,
                            &ctx.services.db_pool,
                            self.dpf_sdk.as_deref(),
                        )
                        .await
                        {
                            Ok(mut extension_services_status)
                                if extension_services_status.configs_synced
                                    == SyncState::Synced
                                    && !extension_services_status
                                        .get_terminated_service_keys()
                                        .is_empty() =>
                            {
                                let mut txn = ctx.services.db_pool.begin().await?;
                                cleanup_terminated_extension_services(
                                    instance,
                                    &mut extension_services_status,
                                    txn.as_mut(),
                                )
                                .await?;
                                txn_opt = Some(txn);
                            }
                            Ok(_) => {}
                            Err(error) => {
                                // The instance is already Ready, so placement drift must not
                                // block unrelated Ready work. The next scan retries it.
                                tracing::warn!(
                                    machine_id = %host_machine_id,
                                    error = %error,
                                    "failed to reconcile DPF Helm chart extension-service placement; will retry"
                                );
                            }
                        }
                    }

                    if is_auto_approved && (reprov_can_be_started || host_firmware_requested) {
                        tracing::info!(machine_id = %host_machine_id, "Auto rebooting host for reprovision/upgrade due to being in approved time period");
                    }

                    if instance.deleted.is_some()
                        || reprov_can_be_started
                        || host_firmware_requested
                        || boot_with_custom_ipxe
                    {
                        for dpu_snapshot in &mh_snapshot.dpu_snapshots {
                            if dpu_snapshot.reprovision_requested.is_some() {
                                // User won't be allowed to clear reprovisioning flag after this.
                                ctx.pending_db_writes.push(
                                    MachineWriteOp::UpdateDpuReprovisionStartTime {
                                        machine_id: dpu_snapshot.id,
                                        time: Utc::now(),
                                    },
                                );
                            }
                        }
                        if mh_snapshot
                            .host_snapshot
                            .host_reprovision_requested
                            .is_some()
                        {
                            ctx.pending_db_writes.push(
                                MachineWriteOp::UpdateHostReprovisionStartTime {
                                    machine_id: mh_snapshot.host_snapshot.id,
                                    time: Utc::now(),
                                },
                            );
                        }

                        // For deletion, sanitize the host BMC first (site-gated,
                        // no-op pass-through to PowerCycle when disabled) via the
                        // FactoryResetBmc sub-flow. The power-state read that used
                        // to gate PowerCycle now happens when that sub-flow reaches
                        // PowerCycle (see `handle_factory_reset_bmc`). For everything
                        // else (reprovision, firmware update, custom PXE), verify
                        // boot config first.
                        let next_state = if instance.deleted.is_some() {
                            ManagedHostState::Assigned {
                                instance_state: InstanceState::HostPlatformConfiguration {
                                    platform_config_state:
                                        HostPlatformConfigurationState::FactoryResetBmc {
                                            reset_state: FactoryResetBmcState::CheckPreconditions,
                                        },
                                },
                            }
                        } else {
                            ManagedHostState::Assigned {
                                instance_state: InstanceState::HostPlatformConfiguration {
                                    platform_config_state:
                                        HostPlatformConfigurationState::UnlockHost {
                                            unlock_host_state: UnlockHostState::DisableLockdown,
                                        },
                                },
                            }
                        };

                        let mut txn = if let Some(txn) = txn_opt.take() {
                            txn
                        } else {
                            ctx.services.db_pool.begin().await?
                        };

                        if host_firmware_requested {
                            let health_override = create_host_update_health_report_hostfw();
                            let machine_id = *host_machine_id;
                            // The health report alert gets generated here, the machine update manager retains responsibilty for clearing it when we're done.
                            db::machine::insert_health_report(
                                &mut txn,
                                &machine_id,
                                HealthReportApplyMode::Merge,
                                &health_override,
                                false,
                            )
                            .await?;
                        }

                        if reprov_can_be_started {
                            let health_override = create_host_update_health_report_dpufw();
                            let machine_id = *host_machine_id;
                            // Mark the Host as in update.
                            db::machine::insert_health_report(
                                &mut txn,
                                &machine_id,
                                HealthReportApplyMode::Merge,
                                &health_override,
                                false,
                            )
                            .await?;
                        }

                        Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
                    } else if let Some(txn) = txn_opt {
                        // Commit extension cleanup before the observer performs
                        // Redfish I/O in a separate attempt.
                        Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
                    } else {
                        let observation_outcome =
                            boot_interface_observation::observe_verified_boot_interface(
                                ctx,
                                mh_snapshot,
                            )
                            .await?;
                        if matches!(
                            &observation_outcome,
                            StateHandlerOutcome::DoNothing { txn: Some(_), .. }
                        ) {
                            // Commit the observation transaction before doing any other Ready work.
                            // The provisioning retry can run during the next controller iteration.
                            return Ok(observation_outcome);
                        }

                        // `use_custom_pxe_on_boot` remains set until Core serves the tenant boot
                        // instructions. Together with missing phone home, it identifies
                        // provisioning that has not reached the tenant iPXE handler.
                        // Operator-managed networks do not use phone home to determine tenant
                        // readiness, so skip this retry.
                        if instance.use_custom_pxe_on_boot
                            && instance.config.os.phone_home_enabled
                            && instance.observations.phone_home_last_contact.is_none()
                            && !instance.config.network.uses_operator_managed_networking()
                        {
                            // Apply the same PXE safety checks as the initial provisioning reboot.
                            // An explicit custom PXE request bypasses them only for its original
                            // reboot, not for an automatic retry.
                            if let Some(reason) = provisioning_pxe_reboot_wait_reason(mh_snapshot)?
                            {
                                return Ok(StateHandlerOutcome::wait(reason.to_string()));
                            }

                            return match trigger_reboot_if_needed_without_power_cycle(
                                &mh_snapshot.host_snapshot,
                                mh_snapshot,
                                None,
                                &self.reachability_params,
                                ctx,
                            )
                            .await
                            {
                                Ok(status) => {
                                    if status.increase_retry_count {
                                        record_provisioning_retry_attempt(
                                            &mh_snapshot.host_snapshot,
                                            ctx,
                                        );
                                    }
                                    Ok(StateHandlerOutcome::wait(format!(
                                        "Waiting for instance provisioning boot. {}",
                                        status.status
                                    )))
                                }
                                Err(error @ StateHandlerError::ManualInterventionRequired(_)) => {
                                    // Intermediate verification attempts return before Ready. If an
                                    // unverified restart reaches this arm, a final verification
                                    // update is queued and must commit before reporting the error.
                                    let restart_verification_must_commit = mh_snapshot
                                        .host_snapshot
                                        .status
                                        .last_reboot_requested
                                        .is_some_and(|reboot| {
                                            reboot.restart_verified == Some(false)
                                        });
                                    if restart_verification_must_commit {
                                        Ok(StateHandlerOutcome::wait(error.to_string()))
                                    } else {
                                        Err(error)
                                    }
                                }
                                Err(error) => {
                                    // Redfish client setup can fail before the power path records an
                                    // attempt. Record the next retry time and clear verification so
                                    // Ready does not retry every controller iteration or verify a
                                    // restart that was never issued.
                                    record_provisioning_retry_attempt(
                                        &mh_snapshot.host_snapshot,
                                        ctx,
                                    );
                                    tracing::warn!(
                                        machine_id = %host_machine_id,
                                        error = %error,
                                        "Failed to retry instance provisioning boot; will retry",
                                    );
                                    Ok(StateHandlerOutcome::wait(format!(
                                        "Failed to retry instance provisioning boot: {error}. Will retry."
                                    )))
                                }
                            };
                        }

                        Ok(observation_outcome)
                    }
                }
                InstanceState::HostPlatformConfiguration {
                    platform_config_state,
                } => {
                    handle_instance_host_platform_config(
                        ctx,
                        mh_snapshot,
                        &self.reachability_params,
                        platform_config_state.clone(),
                    )
                    .await
                }
                InstanceState::WaitingForDpusToUp => {
                    // A zero-DPU host has no DPUs to wait for. Skip the
                    // readiness check and proceed with the rest of the
                    // handler (custom-PXE reboot, termination flow, etc).
                    if mh_snapshot.has_managed_dpus()
                        && !are_dpus_up_trigger_reboot_if_needed(
                            mh_snapshot,
                            &self.reachability_params,
                            ctx,
                        )
                        .await
                    {
                        return Ok(StateHandlerOutcome::wait(
                            "Waiting for DPUs to come up.".to_string(),
                        ));
                    }

                    // If custom_pxe_reboot_requested is set, transition to WaitingForRebootToReady and reboot.
                    // The iPXE handler will then serve the tenant's custom script when the host PXE boots.
                    //
                    // The API sets custom_pxe_reboot_requested when the tenant explicitly requests
                    // "Reboot with Custom iPXE"
                    //
                    // Otherwise, follow the normal termination/reprovision flow through
                    // BootingWithDiscoveryImage.
                    if instance.custom_pxe_reboot_requested {
                        if !instance
                            .config
                            .os
                            .run_provisioning_instructions_on_every_boot
                        {
                            ctx.pending_db_writes
                                .push(MachineWriteOp::UseCustomIpxeOnNextBoot {
                                    machine_id: mh_snapshot.host_snapshot.id,
                                    boot_with_custom_ipxe: true,
                                });
                        }

                        let next_state = ManagedHostState::Assigned {
                            instance_state: InstanceState::WaitingForRebootToReady,
                        };
                        Ok(StateHandlerOutcome::transition(next_state))
                    } else {
                        // Current DPU health can show that Scout's primary network path is not
                        // ready even after every DPU publishes a fresh status. Other flows must
                        // continue to BootingWithDiscoveryImage because that state starts repair.
                        if instance.deleted.is_some()
                            && mh_snapshot.has_managed_dpus()
                            && primary_dpu_has_release_pxe_blocking_network_alert(mh_snapshot)
                        {
                            return Ok(StateHandlerOutcome::wait(
                                DPU_NETWORK_READY_WAIT_REASON.to_string(),
                            ));
                        }

                        handler_host_power_control(
                            mh_snapshot,
                            ctx,
                            SystemPowerControl::ForceRestart,
                        )
                        .await?;
                        let next_state = ManagedHostState::Assigned {
                            instance_state: InstanceState::BootingWithDiscoveryImage {
                                retry: RetryInfo { count: 0 },
                            },
                        };
                        Ok(StateHandlerOutcome::transition(next_state))
                    }
                }
                InstanceState::BootingWithDiscoveryImage { retry } => {
                    if !rebooted(&mh_snapshot.host_snapshot) {
                        let status = trigger_reboot_if_needed(
                            &mh_snapshot.host_snapshot,
                            mh_snapshot,
                            // can't send 0. 0 will force power-off as cycle calculator.
                            Some(retry.count as i64 + 1),
                            &self.reachability_params,
                            ctx,
                        )
                        .await?;

                        let st = if status.increase_retry_count {
                            let next_state = ManagedHostState::Assigned {
                                instance_state: InstanceState::BootingWithDiscoveryImage {
                                    retry: RetryInfo {
                                        count: retry.count + 1,
                                    },
                                },
                            };
                            StateHandlerOutcome::transition(next_state)
                        } else {
                            StateHandlerOutcome::wait(status.status)
                        };
                        return Ok(st);
                    }

                    // `trigger_reboot_if_needed` stops rebooting after 15 attempts, so this counter
                    // stops increasing at the same limit.
                    ctx.metrics
                        .machine_reboot_attempts_in_booting_with_discovery_image =
                        Some(retry.count + 1);

                    // In case state is triggered for delete instance handling, follow that path.
                    if instance.deleted.is_some() {
                        let next_state = ManagedHostState::Assigned {
                            instance_state: InstanceState::SwitchToAdminNetwork,
                        };
                        return Ok(StateHandlerOutcome::transition(next_state));
                    }

                    // If we are here, DPU reprov MUST have been be requested.
                    if dpu_reprovisioning_needed(&mh_snapshot.dpu_snapshots) {
                        // All DPUs must have same value for this parameter. All DPUs are updated
                        // together grpc API or automatic updater.
                        // TODO: multidpu: Keep it at some common place to avoid duplicates.
                        let mut dpus_for_reprov = vec![];
                        for dpu_snapshot in &mh_snapshot.dpu_snapshots {
                            if dpu_snapshot.reprovision_requested.is_some() {
                                handler_restart_dpu(
                                    dpu_snapshot,
                                    ctx,
                                    mh_snapshot.host_snapshot.config.dpf.used_for_ingestion,
                                )
                                .await?;
                                dpus_for_reprov.push(dpu_snapshot);
                            }
                        }

                        set_managed_host_topology_update_needed(
                            ctx.pending_db_writes,
                            &mh_snapshot.host_snapshot,
                            &dpus_for_reprov,
                        );

                        let next_state = ReprovisionState::next_substate_based_on_bfb_support(
                            self.enable_secure_boot,
                            mh_snapshot,
                            ctx.services.site_config.dpf_enabled,
                        )
                        .next_state_with_all_dpus_updated(
                            &mh_snapshot.managed_state,
                            &mh_snapshot.dpu_snapshots,
                            dpus_for_reprov
                                .into_iter()
                                .map(Machine::dpu_machine_id)
                                .collect::<Result<Vec<_>, _>>()?,
                        )?;
                        Ok(StateHandlerOutcome::transition(next_state))
                    } else if mh_snapshot
                        .host_snapshot
                        .host_reprovision_requested
                        .is_some()
                    {
                        let reprovision_state = if is_rack_level_reprovisioning(mh_snapshot) {
                            HostReprovisionState::WaitingForRackFirmwareUpgrade
                        } else {
                            HostReprovisionState::CheckingFirmwareV2 {
                                firmware_type: None,
                                firmware_number: None,
                            }
                        };
                        Ok(StateHandlerOutcome::transition(
                            ManagedHostState::Assigned {
                                instance_state: InstanceState::HostReprovision {
                                    reprovision_state,
                                },
                            },
                        ))
                    } else {
                        Ok(StateHandlerOutcome::wait(
                            "Don't know how did we reach here.".to_string(),
                        ))
                    }
                }

                InstanceState::SwitchToAdminNetwork => {
                    // Tenant is gone, switch back to admin by setting `use_admin_network`
                    // to true on the host-level config, which, just like in the other
                    // direction, version bumps the entire machine group (host + DPUs),
                    // so they report "out of sync" until agents poll + apply + report,
                    // where WaitingForNetworkReconfig waits on that.
                    let mut txn = ctx.services.db_pool.begin().await?;
                    let host_version = mh_snapshot.host_snapshot.network_config.version;
                    let mut host_netconf = mh_snapshot.host_snapshot.network_config.value.clone();
                    let old_use_admin_network = host_netconf.use_admin_network;
                    host_netconf.use_admin_network = Some(true);
                    let updated = db::machine::try_update_network_config(
                        &mut txn,
                        &mh_snapshot.host_snapshot.id,
                        host_version,
                        &host_netconf,
                    )
                    .await?;

                    // Set use_admin_network_changed if we want to reboot
                    // ovs on admin network change.
                    if updated
                        && old_use_admin_network != host_netconf.use_admin_network
                        && ctx
                            .services
                            .site_config
                            .restart_ovs_on_use_admin_network_change
                    {
                        process_dpu_use_admin_network_state_change(&mut txn, mh_snapshot).await?;
                    }

                    // Bump each DPA interface's config version so the DPA State Controller
                    // re-evaluates and sends SetVNI commands with VNI zero.
                    for dpa_interface in &mh_snapshot.dpa_interface_snapshots {
                        let (mut netconf, version) = dpa_interface.network_config.clone().take();
                        netconf.use_admin_network = Some(true);
                        db::dpa_interface::try_update_network_config(
                            &mut txn,
                            &dpa_interface.id,
                            version,
                            &netconf,
                        )
                        .await?;
                    }

                    let next_state = ManagedHostState::Assigned {
                        instance_state: InstanceState::WaitingForNetworkReconfig,
                    };
                    Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
                }
                InstanceState::WaitingForNetworkReconfig => {
                    // Has forge-dpu-agent applied the new network config so that
                    // we are back on the admin network?
                    if !mh_snapshot.managed_host_network_config_version_synced() {
                        return Ok(StateHandlerOutcome::wait(
                                    "Waiting for DPU agent(s) to apply network config and report healthy network"
                                        .to_string()
                                ));
                    }

                    // Extension services should be terminated on all DPUs
                    if mh_snapshot.has_managed_dpus() {
                        let extension_services_status = get_extension_services_status(
                            mh_snapshot,
                            instance,
                            &ctx.services.db_pool,
                            self.dpf_sdk.as_deref(),
                        )
                        .await?;
                        if !extension_service::are_all_extension_services_terminated(
                            &extension_services_status,
                        ) {
                            return Ok(StateHandlerOutcome::wait(
                                "Waiting for extension services to be terminated on all required DPUs."
                                    .to_string(),
                            ));
                        }
                    }

                    // Check each DPA interface associated with the machine to make sure the DPA NIC has updated
                    // its network config (setting VNI to zero in this case).
                    if ctx.services.site_config.ewethers_enabled {
                        for dpa_interface in &mh_snapshot.dpa_interface_snapshots {
                            // We're heading back to admin and a DPA still in
                            // Provisioning has nothing to ack -- it never
                            // applied a tenant-network SetVNI in the first
                            // place. Treat it as trivially synced so we don't
                            // block the state machine on uninitialized DPAs.
                            if dpa_interface.controller_state.value
                                == DpaInterfaceControllerState::Provisioning
                            {
                                continue;
                            }
                            if !dpa_interface.managed_host_network_config_version_synced(
                                &None,
                                &mh_snapshot.host_snapshot.status.spx_status_observation,
                            ) {
                                return Ok(StateHandlerOutcome::wait(
                                            "Waiting for DPA agent(s) to apply network config and report healthy network"
                                                .to_string()
                                        ));
                            }
                        }
                    }

                    check_host_health_for_alerts(mh_snapshot)?;

                    // Check whether IB config is removed
                    match ib_config_synced(
                        mh_snapshot
                            .host_snapshot
                            .status
                            .infiniband_status_observation
                            .as_ref(),
                        Some(&instance.config.infiniband),
                        false,
                    ) {
                        Ok(()) => {
                            // Config is synced, proceed with termination
                        }
                        Err(IbConfigNotSyncedReason::PortStateUnobservable { guids, details }) => {
                            tracing::warn!(
                                instance_id = %instance.id,
                                machine_id = %host_machine_id,
                                guids = ?guids,
                                reason = %details,
                                "IB ports not observable during termination - IB Monitor will unbind"
                            );

                            // Collect GUIDs for cleanup
                            // TODO: Include fabric name for multi-fabric deployments
                            let message = format!(
                                "IB port cleanup pending - IB Monitor will unbind. GUIDs: {}",
                                guids.join("; ")
                            );

                            // Create health report with alert that will prevent re-allocation
                            // IB Monitor will unbind before clearing
                            let health_report = HealthReport {
                                source: "ib-cleanup-validation".to_string(),
                                triggered_by: None,
                                observed_at: Some(chrono::Utc::now()),
                                alerts: vec![HealthProbeAlert {
                                    id: HealthProbeId::from_str("IbCleanupPending")
                                        .expect("valid probe id"),
                                    target: None,
                                    in_alert_since: Some(chrono::Utc::now()),
                                    message,
                                    tenant_message: None,
                                    classifications: vec![
                                        HealthAlertClassification::prevent_allocations(),
                                    ],
                                }],
                                successes: vec![],
                            };

                            // Use health report override instead of state_controller_health_report field
                            // This is ok to defer into pending_db_writes because we're passing
                            // `no_overwrite: false`, meaning we will overwrite any overrides
                            // already in place.
                            ctx.pending_db_writes
                                .push(MachineWriteOp::InsertMachineHealthReport {
                                    machine_id: *host_machine_id,
                                    mode: health_report::HealthReportApplyMode::Merge,
                                    health_report,
                                });

                            tracing::info!(
                                machine_id = %host_machine_id,
                                guids = ?guids,
                                "IbCleanupPending alert created - IB Monitor will handle unbind and clear alert"
                            );

                            // Termination proceeds - IB Monitor will handle cleanup
                        }
                        Err(other_reason) => {
                            return Ok(StateHandlerOutcome::wait(format!(
                                "Waiting for IB config to be removed (Reason: {})",
                                other_reason
                            )));
                        }
                    }

                    // TODO: TPM cleanup
                    // Reboot host
                    handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart)
                        .await?;

                    // Deleting an instance and marking vpc segments deleted must be done together.
                    // If segments are marked deleted and instance is not deleted (may be due to redfish failure),
                    // network segment handler will delete those segments forcefully.
                    // if instance is deleted before, we won't get network segment details as these
                    // details are stored in instance's network config which is deleted.

                    // Delete from database now. Once done, reboot and move to next state.
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db::instance::delete(instance.id, &mut txn)
                        .await
                        .map_err(|err| StateHandlerError::GenericError(err.into()))?;

                    release_network_segments_with_vpc_prefix(
                        &instance.config.network.interfaces,
                        &mut txn,
                    )
                    .await?;

                    // Free up all loopback IPs allocated for this instance.
                    release_vpc_dpu_loopback(mh_snapshot, self.common_pools.as_deref(), &mut txn)
                        .await?;

                    let next_state = ManagedHostState::PostAssignedMeasuring {
                        attestation_mode: AttestationMode::MeasuredBoot {
                            measuring_state: MeasuringState::WaitingForMeasurements,
                        },
                    };

                    Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
                }
                InstanceState::DPUReprovision { .. } => {
                    // Reaching DPUReprovision with no DPUs is technically a
                    // bug/violation; the reprovision branch should have been
                    // skipped upstream. But, without this guard, the empty loop
                    // below falls through to `do_nothing()` and the host
                    // would/could sit in `DPUReprovision` forever.
                    if !mh_snapshot.has_managed_dpus() {
                        return Err(StateHandlerError::GenericError(eyre!(
                            "DPUReprovision state entered on zero-DPU host {host_machine_id}; reprovision requires DPUs"
                        )));
                    }

                    let fw_config_snapshot = self.hardware_models.create_snapshot();
                    for dpu_snapshot in &mh_snapshot.dpu_snapshots {
                        if let outcome @ StateHandlerOutcome::Transition { .. } =
                            handle_dpu_reprovision(
                                mh_snapshot,
                                &self.reachability_params,
                                &InstanceNextStateResolver,
                                dpu_snapshot,
                                ctx,
                                &fw_config_snapshot,
                                self.dpf_sdk.as_deref(),
                            )
                            .await?
                        {
                            return Ok(outcome);
                        }
                    }
                    Ok(StateHandlerOutcome::do_nothing())
                }
                InstanceState::Failed {
                    details,
                    machine_id,
                } => match details.cause {
                    FailureCause::BiosSetupFailed { .. } if machine_id.machine_type().is_host() => {
                        let recovered = ManagedHostState::Assigned {
                            instance_state: InstanceState::HostPlatformConfiguration {
                                platform_config_state:
                                    HostPlatformConfigurationState::SetBootOrder {
                                        set_boot_order_info: initial_set_boot_order_info(),
                                    },
                            },
                        };
                        handle_bios_setup_failed_recovery(ctx, mh_snapshot, None, recovered).await
                    }
                    _ => {
                        // Only way to proceed for other causes is to
                        // 1. Force-delete the machine.
                        // 2. If failed during reprovision, fix the config/hw issue and
                        //    retrigger DPU reprovision.
                        tracing::warn!(
                            instance_id = %instance.id,
                            machine_id = %host_machine_id,
                            failure_details = ?details,
                            failed_machine_id = %machine_id,
                            "Instance machine stuck in failed state"
                        );
                        Ok(StateHandlerOutcome::do_nothing())
                    }
                },
                InstanceState::HostReprovision { .. } => {
                    self.host_upgrade
                        .handle_host_reprovision(
                            mh_snapshot,
                            ctx,
                            host_machine_id,
                            HostFirmwareScenario::Instance,
                        )
                        .await
                }
                InstanceState::NetworkConfigUpdate {
                    network_config_update_state,
                } => {
                    handle_instance_network_config_update_request(
                        mh_snapshot,
                        network_config_update_state,
                        instance,
                        ctx,
                        &self.common_pools,
                    )
                    .await
                }
                InstanceState::DpaProvisioning => {
                    // An instance is being created. The host was already flipped
                    // to tenant network in the Ready -> Assigned transition; here
                    // we just bump each DPA interface's config version so the
                    // DPA state controller re-evaluates with the new host value
                    // (READY -> WaitingForSetVNI, triggering SetVNI).

                    // Note that we have to defer setting use_admin_network for the DPUs
                    // till after DPA provisioning is complete. This is due to the fact
                    // that we have to interact with scout to unlock/apply firmware/lock
                    // the card. If we switch the DPUs also out of admin network, we will
                    // no longer be able to interact with scout.

                    let mut txn = ctx.services.db_pool.begin().await?;
                    if ctx.services.site_config.ewethers_enabled {
                        for dpa_interface in &mh_snapshot.dpa_interface_snapshots {
                            let (mut netconf, version) =
                                dpa_interface.network_config.clone().take();
                            netconf.use_admin_network = Some(false);
                            db::dpa_interface::try_update_network_config(
                                &mut txn,
                                &dpa_interface.id,
                                version,
                                &netconf,
                            )
                            .await?;
                        }
                    }
                    let next_state = ManagedHostState::Assigned {
                        instance_state: InstanceState::WaitingForDpaToBeReady,
                    };
                    Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
                }
                InstanceState::WaitingForDpaToBeReady => {
                    // Check each DPA interface to see if it has acted on updating the network config.
                    // This involves the DPA State Machine sending SetVNI commands to the NICs, and getting
                    // an ACK. If any of the interfaces has not yet heard back the ACk, we will continue to
                    // be in the current state.

                    if ctx.services.site_config.ewethers_enabled {
                        for dpa_interface in &mh_snapshot.dpa_interface_snapshots {
                            if !dpa_interface.managed_host_network_config_version_synced(
                                &mh_snapshot.instance,
                                &mh_snapshot.host_snapshot.status.spx_status_observation,
                            ) {
                                return Ok(StateHandlerOutcome::wait(format!(
                                    "Waiting for DPA agent {dpa_id} to apply network config and report healthy network",
                                    dpa_id = dpa_interface.id,
                                )));
                            }
                        }
                    }

                    let mut txn = ctx.services.db_pool.begin().await?;
                    let host_version = mh_snapshot.host_snapshot.network_config.version;
                    let mut host_netconf = mh_snapshot.host_snapshot.network_config.value.clone();
                    let old_use_admin_network = host_netconf.use_admin_network;
                    host_netconf.use_admin_network = Some(false);
                    let updated = db::machine::try_update_network_config(
                        &mut txn,
                        &mh_snapshot.host_snapshot.id,
                        host_version,
                        &host_netconf,
                    )
                    .await?;

                    // Set use_admin_network_changed if we want to reboot
                    // ovs on admin network change.
                    if updated
                        && old_use_admin_network != host_netconf.use_admin_network
                        && ctx
                            .services
                            .site_config
                            .restart_ovs_on_use_admin_network_change
                    {
                        process_dpu_use_admin_network_state_change(&mut txn, mh_snapshot).await?;
                    }

                    // The host was already flipped to tenant network in the
                    // Ready -> Assigned transition; that write fanned out via
                    // `try_update_network_config`'s group sync to bump every
                    // DPU's version too, so no DPU bumps are needed here.
                    let next_state = ManagedHostState::Assigned {
                        instance_state: InstanceState::WaitingForNetworkSegmentToBeReady,
                    };
                    return Ok(StateHandlerOutcome::transition(next_state).with_txn(txn));
                }
            }
        } else {
            // We are not in Assigned state. Should this be Err(StateHandlerError::InvalidHostState)?
            Ok(StateHandlerOutcome::do_nothing())
        }
    }
}

// Process the host's use_admin_network flag change and selectively flag
// DPUs that undergo an actual network mode change with the
// use_admin_network_changed flag if restart_ovs_on_use_admin_network_change
// is true.
// Not every DPU participates in tenant networking — only those with instance
// interface configs assigned to them. DPUs without tenant interfaces remain on
// the admin network regardless of the host-level toggle.
async fn process_dpu_use_admin_network_state_change(
    txn: &mut PgConnection,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<(), StateHandlerError> {
    tracing::info!(
        machine_id = %mh_snapshot.host_snapshot.id,
        "Set use_admin_network_changed flag as host has changed use_admin_network state and site-restart-ovs is set"
    );

    // Determine which DPUs have tenant interface configs. A DPU matches if:
    //  - the interface config has no device_locator and the DPU is the
    // primary, OR
    //  - the interface config's device_locator matches this DPU's PCI device
    //
    // Only these DPUs actually transition between admin and tenant network.
    // On allocate: they switch from admin → tenant.
    // On release: they switch from tenant → admin.
    // DPUs without tenant interfaces never change mode — skip them.
    let interface_configs = mh_snapshot
        .instance
        .as_ref()
        .map(|i| i.config.network.interfaces.clone())
        .unwrap_or_default();

    let primary_dpu_id = mh_snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|i| i.primary_interface)
        .and_then(|i| i.attached_dpu_machine_id);

    for dpu in &mh_snapshot.dpu_snapshots {
        let dpu_id = dpu.dpu_machine_id()?;
        let dpu_has_tenant_interface_config = interface_configs.iter().any(|cfg| {
            let is_primary = primary_dpu_id == Some(dpu_id);
            (cfg.device_locator.is_none() && is_primary)
                || (cfg.device_locator.is_some()
                    && mh_snapshot
                        .host_snapshot
                        .get_device_locator_for_dpu_id(&dpu_id)
                        .ok()
                        .as_ref()
                        == cfg.device_locator.as_ref())
        });

        if dpu_has_tenant_interface_config {
            tracing::info!("Set DPU use_admin_network_changed flag for dpu {}", &dpu.id);
            db::machine::set_use_admin_network_changed(txn, &dpu.id, true).await?;
        }
    }

    Ok(())
}

async fn handle_instance_network_config_update_request(
    mh_snapshot: &ManagedHostStateSnapshot,
    network_config_update_state: &NetworkConfigUpdateState,
    instance: &InstanceSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    common_pools: &Option<Arc<CommonPools>>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match network_config_update_state {
        NetworkConfigUpdateState::WaitingForNetworkSegmentToBeReady => {
            let next_state = ManagedHostState::Assigned {
                instance_state: InstanceState::NetworkConfigUpdate {
                    network_config_update_state: NetworkConfigUpdateState::WaitingForConfigSynced,
                },
            };

            let Some(update_request) = &instance.update_network_config_request else {
                return Err(StateHandlerError::GenericError(eyre::eyre!(
                    "network config update request is missing from db. instance: {}",
                    instance.id
                )));
            };

            let network_segment_ids_with_vpc = update_request
                .new_config
                .interfaces
                .iter()
                .filter_map(InstanceInterfaceConfig::generated_network_segment_id)
                .collect_vec();

            // Generated VPC-prefix segments must be ready before promotion.
            if !network_segment_ids_with_vpc.is_empty() {
                let network_segments_are_ready = db::network_segment::are_network_segments_ready(
                    &mut ctx.services.db_reader,
                    &network_segment_ids_with_vpc,
                )
                .await?;
                if !network_segments_are_ready {
                    return Ok(StateHandlerOutcome::wait(
                        "Waiting for all segments to come in ready state.".to_string(),
                    ));
                }
            }

            // Update requested network config and increment version.
            let mut txn = ctx.services.db_pool.begin().await?;
            db::instance::update_network_config(
                txn.as_mut(),
                instance.id,
                instance.network_config_version,
                &update_request.new_config,
                true,
            )
            .await?;

            Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
        }
        NetworkConfigUpdateState::WaitingForConfigSynced => {
            let next_state = ManagedHostState::Assigned {
                instance_state: InstanceState::NetworkConfigUpdate {
                    network_config_update_state: NetworkConfigUpdateState::ReleaseOldResources,
                },
            };

            Ok(
                match check_instance_network_synced_and_dpu_healthy(instance, mh_snapshot)? {
                    InstanceNetworkSyncStatus::InstanceNetworkObservationNotAvailable(
                        missing_dpus,
                    ) => StateHandlerOutcome::wait(format!(
                        "Waiting for DPU agents to apply initial network config for DPUs: {}",
                        missing_dpus.iter().map(|dpu| dpu.to_string()).join(", ")
                    )),
                    InstanceNetworkSyncStatus::ZeroDpuNoObservationNeeded
                    | InstanceNetworkSyncStatus::InstanceNetworkSynced => {
                        StateHandlerOutcome::transition(next_state)
                    }
                    InstanceNetworkSyncStatus::InstanceNetworkNotSynced(outdated_dpus) => {
                        StateHandlerOutcome::wait(format!(
                            "Waiting for DPU agent to apply most recent network config for DPUs: {}",
                            outdated_dpus.iter().map(|dpu| dpu.to_string()).join(", ")
                        ))
                    }
                },
            )
        }
        NetworkConfigUpdateState::ReleaseOldResources => {
            let mut txn = ctx.services.db_pool.begin().await?;
            // Identify all the resources which have to be released.
            // Release Ips.
            // Release segments.
            // Release VpcDpuLoopbackIps.
            // Free the update_network_config_request field.
            let Some(update_request) = &instance.update_network_config_request else {
                return Err(StateHandlerError::GenericError(eyre::eyre!(
                    "network config update request is missing from db. instance: {}",
                    instance.id
                )));
            };

            // Logically new_config is current_config now.
            let mut new_config = update_request.new_config.clone();
            let copied_resources = new_config.copy_existing_resources(&update_request.old_config);

            let resources_to_be_released = update_request
                .old_config
                .interfaces
                .iter()
                .filter(|x| !copied_resources.contains(x))
                .cloned()
                .collect_vec();

            if !resources_to_be_released.is_empty() {
                // Resolve VPC membership before old VPC-prefix segments are marked deleted.
                let old_vpc_ids =
                    vpc_ids_for_interfaces(&update_request.old_config.interfaces, &mut txn).await?;
                let new_vpc_ids =
                    vpc_ids_for_interfaces(&update_request.new_config.interfaces, &mut txn).await?;
                let released_vpc_ids = old_vpc_ids.difference(&new_vpc_ids).copied().collect_vec();

                let addresses = resources_to_be_released
                    .iter()
                    .flat_map(|interface| {
                        interface
                            .ip_addrs
                            .values()
                            .map(move |address| (interface.network_segment_id, *address))
                    })
                    .map(|(segment_id, address)| {
                        Ok((
                            segment_id.ok_or_else(|| {
                                StateHandlerError::GenericError(eyre::eyre!(
                                    "instance {} has allocated addresses without a network segment",
                                    instance.id,
                                ))
                            })?,
                            address,
                        ))
                    })
                    .collect::<Result<Vec<_>, StateHandlerError>>()?;

                tracing::info!(
                    instance_id = %instance.id,
                    addresses = ?addresses,
                    "releasing network resources",
                );
                db::instance_address::delete_addresses_for_instance(
                    &mut txn,
                    instance.id,
                    &addresses,
                )
                .await?;
                release_network_segments_with_vpc_prefix(&resources_to_be_released, &mut txn)
                    .await?;
                release_vpc_dpu_loopback_for_vpcs(
                    mh_snapshot,
                    common_pools.as_deref(),
                    &mut txn,
                    &released_vpc_ids,
                )
                .await?;
            }
            db::instance::delete_update_network_config_request(&instance.id, &mut txn).await?;
            let next_state = ManagedHostState::Assigned {
                instance_state: InstanceState::Ready,
            };
            Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
        }
    }
}

/// Checks if an instance's network is synced and its DPU is healthy.
///
/// This function compares the expected network configuration version with the actual version.
/// It also checks the health of the DPU by calling `check_host_health_for_alerts`.
///
/// # Notes
/// This function currently does not support multi-DPU handling.
fn check_instance_network_synced_and_dpu_healthy(
    instance: &InstanceSnapshot,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<InstanceNetworkSyncStatus, StateHandlerError> {
    if mh_snapshot
        .host_snapshot
        .associated_dpu_machine_ids()
        .is_empty()
    {
        tracing::info!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "Skipping network config because machine has no DPUs"
        );
        return Ok(InstanceNetworkSyncStatus::ZeroDpuNoObservationNeeded);
    }

    let device_locators: Vec<DeviceLocator> = instance
        .config
        .network
        .interfaces
        .iter()
        .filter_map(|i| i.device_locator.clone())
        .collect();

    let maps = mh_snapshot
        .host_snapshot
        .get_dpu_device_and_id_mappings()
        .unwrap_or_else(|_| (HashMap::default(), HashMap::default()));

    let legacy_physical_interface_count = instance
        .config
        .network
        .interfaces
        .iter()
        .filter(|iface| {
            iface.function_id == InterfaceFunctionId::Physical {} && iface.device_locator.is_none()
        })
        .count();

    let use_primary_dpu_only = legacy_physical_interface_count > 0
        || device_locators.is_empty()
        || maps.0.is_empty()
        || maps.1.is_empty();

    let dpu_machine_ids: Vec<DpuMachineId> = if use_primary_dpu_only {
        if legacy_physical_interface_count != 1 {
            return Err(StateHandlerError::GenericError(eyre!(
                "more than one interface configured when only the primary dpu is allowed"
            )));
        }
        // allow primary dpu to be used when using one config with no device_locators
        match mh_snapshot
            .host_snapshot
            .status
            .interfaces
            .iter()
            .find(|iface| iface.primary_interface)
            .and_then(|iface| iface.attached_dpu_machine_id)
        {
            Some(primary_dpu_id) => vec![primary_dpu_id],
            None => {
                return Err(StateHandlerError::GenericError(eyre!(
                    "could not find primary dpu id"
                )));
            }
        }
    } else {
        if maps.0.is_empty() || maps.1.is_empty() {
            return Err(StateHandlerError::GenericError(eyre!(
                "no interface device locators for when using multiple interfaces"
            )));
        }

        let id_to_device_map = maps.0;
        let device_to_id_map = maps.1;
        // filter out dpus that do not have interfaces configured
        mh_snapshot
            .host_snapshot
            .associated_dpu_machine_ids()
            .iter()
            .filter(|dpu_machine_id| {
                if let Some(device) = id_to_device_map.get(dpu_machine_id) {
                    tracing::info!(
                        %device,
                        dpu_machine_id = %dpu_machine_id,
                        "Found DPU device"
                    );
                    if let Some(id_vec) = device_to_id_map.get(device)
                        && let Some(device_instance) =
                            id_vec.iter().position(|id| id == *dpu_machine_id)
                    {
                        tracing::info!(
                            device_instance,
                            dpu_machine_id = %dpu_machine_id,
                            "Found DPU device instance"
                        );
                        let device_locator = DeviceLocator {
                            device: device.clone(),
                            device_instance,
                        };
                        return instance.config.network.interfaces.iter().any(|i| {
                            i.device_locator
                                .as_ref()
                                .is_some_and(|dl| dl == &device_locator)
                        });
                    }
                }
                false
            })
            .copied()
            .collect()
    };

    if instance.observations.network.len() != dpu_machine_ids.len() {
        tracing::info!(
            network_observation_count = instance.observations.network.len(),
            dpu_count = dpu_machine_ids.len(),
            "network observation count does not match DPU count"
        );

        let mut missing_dpus = Vec::default();
        for dpu_id in dpu_machine_ids {
            tracing::info!(dpu_machine_id = %dpu_id, "checking dpu");
            if !instance.observations.network.contains_key(&dpu_id) {
                tracing::info!("missing");
                missing_dpus.push(dpu_id);
            }
        }
        return Ok(
            InstanceNetworkSyncStatus::InstanceNetworkObservationNotAvailable(
                missing_dpus.into_iter().map(Into::into).collect(),
            ),
        );
    }
    // Check instance network config has been applied
    let expected = &instance.network_config_version;

    let mut outdated_dpus = Vec::default();
    for (dpu_machine_id, network_obs) in &instance.observations.network {
        if &network_obs.config_version != expected {
            outdated_dpus.push(*dpu_machine_id);
        }
    }

    if !outdated_dpus.is_empty() {
        return Ok(InstanceNetworkSyncStatus::InstanceNetworkNotSynced(
            outdated_dpus.into_iter().map(Into::into).collect(),
        ));
    }

    check_host_health_for_alerts(mh_snapshot)?;
    Ok(InstanceNetworkSyncStatus::InstanceNetworkSynced)
}

pub async fn release_vpc_dpu_loopback(
    mh_snapshot: &ManagedHostStateSnapshot,
    common_pools: Option<&CommonPools>,
    txn: &mut PgConnection,
) -> Result<(), StateHandlerError> {
    for dpu_snapshot in &mh_snapshot.dpu_snapshots {
        if let Some(common_pools) = common_pools {
            let dpu_id = dpu_snapshot.dpu_machine_id()?;
            db::vpc_dpu_loopback::delete_and_deallocate(common_pools, &dpu_id, txn, false)
                .await
                .map_err(|e| StateHandlerError::ResourceCleanupError {
                    resource: "VpcLoopbackIp",
                    error: e.to_string(),
                })?;
        }
    }

    Ok(())
}

/// Releases VPC DPU loopbacks for selected VPCs on every DPU in the managed host.
async fn release_vpc_dpu_loopback_for_vpcs(
    mh_snapshot: &ManagedHostStateSnapshot,
    common_pools: Option<&CommonPools>,
    txn: &mut PgConnection,
    vpc_ids: &[VpcId],
) -> Result<(), StateHandlerError> {
    let Some(common_pools) = common_pools else {
        return Ok(());
    };

    if vpc_ids.is_empty() {
        return Ok(());
    }

    // Release the removed VPC loopbacks from every DPU that may have rendered them.
    for dpu_snapshot in &mh_snapshot.dpu_snapshots {
        let dpu_id = dpu_snapshot.dpu_machine_id()?;
        db::vpc_dpu_loopback::delete_and_deallocate_for_vpcs(common_pools, &dpu_id, vpc_ids, txn)
            .await
            .map_err(|e| StateHandlerError::ResourceCleanupError {
                resource: "VpcLoopbackIp",
                error: e.to_string(),
            })?;
    }

    Ok(())
}

/// Returns the VPC IDs referenced by the assigned network segments on these interfaces.
async fn vpc_ids_for_interfaces(
    interfaces: &[InstanceInterfaceConfig],
    txn: &mut PgConnection,
) -> Result<HashSet<VpcId>, StateHandlerError> {
    let segment_ids = interfaces
        .iter()
        .filter_map(|iface| iface.network_segment_id)
        .collect_vec();

    if segment_ids.is_empty() {
        return Ok(HashSet::new());
    }

    // Load segment metadata so VPC-prefix and direct segment interfaces share one path.
    let segments = db::network_segment::find_by(
        txn,
        db::ObjectColumnFilter::List(db::network_segment::IdColumn, &segment_ids),
        model::network_segment::NetworkSegmentSearchConfig::default(),
    )
    .await?;

    Ok(segments
        .into_iter()
        .filter_map(|segment| segment.config.vpc_id)
        .collect())
}

async fn release_network_segments_with_vpc_prefix(
    interfaces: &[InstanceInterfaceConfig],
    txn: &mut PgConnection,
) -> Result<(), StateHandlerError> {
    let network_segment_ids_with_vpc = interfaces
        .iter()
        .filter_map(InstanceInterfaceConfig::generated_network_segment_id)
        .collect_vec();

    // Mark generated VPC-prefix segments ready for deletion.
    if !network_segment_ids_with_vpc.is_empty() {
        db::network_segment::mark_as_deleted_no_validation(txn, &network_segment_ids_with_vpc)
            .await
            .map_err(|err| StateHandlerError::ResourceCleanupError {
                resource: "network_segment",
                error: err.to_string(),
            })?;
    }

    Ok(())
}

#[derive(Debug)]
enum HostFirmwareScenario {
    Ready,
    Instance,
}

impl HostFirmwareScenario {
    fn actual_new_state(
        &self,
        reprovision_state: HostReprovisionState,
        host_retry_count: u32,
    ) -> ManagedHostState {
        match self {
            HostFirmwareScenario::Ready => ManagedHostState::HostReprovision {
                reprovision_state,
                retry_count: host_retry_count,
            },
            HostFirmwareScenario::Instance => ManagedHostState::Assigned {
                instance_state: InstanceState::HostReprovision { reprovision_state },
            },
        }
    }

    fn complete_state(&self) -> ManagedHostState {
        match self {
            HostFirmwareScenario::Ready => ManagedHostState::Ready,
            HostFirmwareScenario::Instance => ManagedHostState::Assigned {
                instance_state: InstanceState::Ready,
            },
        }
    }
}

#[derive(Debug, Clone)]
enum UploadResult {
    Success { task_id: String },
    Failure,
}

struct HostUpgradeState {
    parsed_hosts: Arc<FirmwareConfig>,
    downloader: FirmwareDownloader,
    upload_limiter: Arc<Semaphore>,
    no_firmware_update_reset_retries: bool,
    instance_autoreboot_period: Option<TimePeriod>,
    upgrade_script_state: Arc<UpdateScriptManager>,
    credential_reader: Option<Arc<dyn CredentialReader>>,
    async_firmware_uploader: Arc<AsyncFirmwareUploader>,
    hgx_bmc_gpu_reboot_delay: tokio::time::Duration,
}

impl std::fmt::Debug for HostUpgradeState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "HostUpgradeState: parsed_hosts: {:?} downloader: {:?} upload_limiter: {:?} no_firmware_update_reset_retries: {:?} instance_autoreboot_period: {:?}, upgrade_script_state: {:?}",
            self.parsed_hosts,
            self.downloader,
            self.upload_limiter,
            self.no_firmware_update_reset_retries,
            self.instance_autoreboot_period,
            self.upgrade_script_state
        )
    }
}

/// If the machine's parent rack is in `RackState::Error`, clear
/// `host_reprovisioning_requested` and short-circuit back to the machine's
/// pre-reprovisioning steady state (`Ready`, or `Assigned { instance_state:
/// Ready }` if currently assigned). The rack will never advance the
/// remaining `HostReprovision` sub-states once it has bailed out.
///
/// Only applies to rack-level reprovisioning requests; non-rack-initiated
/// reprovisions are independent of the rack's lifecycle.
async fn rack_failed_abort_host_reprovision_outcome(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    machine_id: &MachineId,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    if !is_rack_level_reprovisioning(state) {
        return Ok(None);
    }

    let Some(rack_id) = state.host_snapshot.rack_id.as_ref() else {
        return Ok(None);
    };

    let mut conn = ctx.services.db_pool.acquire().await?;
    let racks = db::rack::find_by(
        conn.as_mut(),
        db::ObjectColumnFilter::One(db::rack::IdColumn, rack_id),
    )
    .await?;
    drop(conn);
    let Some(rack) = racks.into_iter().next() else {
        return Ok(None);
    };
    if !matches!(
        rack.controller_state.value,
        model::rack::RackState::Error { .. }
    ) {
        return Ok(None);
    }

    let target_state = match &state.managed_state {
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostReprovision { .. },
        } => ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
        _ => ManagedHostState::Ready,
    };

    tracing::info!(
        machine_id = %machine_id,
        rack_id = %rack_id,
        previous_state = ?state.managed_state,
        next_state = ?target_state,
        "Rack is in Error; aborting machine HostReprovision and returning to Ready",
    );

    let mut txn = ctx.services.db_pool.begin().await?;
    db::host_machine_update::clear_host_reprovisioning_request(txn.as_mut(), machine_id).await?;
    Ok(Some(
        StateHandlerOutcome::transition(target_state).with_txn(txn),
    ))
}

impl HostUpgradeState {
    async fn host_firmware_config_snapshot(
        &self,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<FirmwareConfigSnapshot, StateHandlerError> {
        let host_firmware_configs =
            db::host_firmware_config::list_configs(&ctx.services.db_pool).await?;

        Ok(self
            .parsed_hosts
            .create_snapshot_with_overrides(host_firmware_configs))
    }

    // Handles when in HostReprovisioning or when entering it
    async fn handle_host_reprovision(
        &self,
        state: &mut ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        machine_id: &MachineId,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        if let Some(outcome) =
            rack_failed_abort_host_reprovision_outcome(state, ctx, machine_id).await?
        {
            return Ok(outcome);
        }

        // Treat Ready (but flagged to do updates) the same as HostReprovisionState/CheckingFirmware
        let original_state = &state.managed_state.clone();
        let (mut host_reprovision_state, mut retry_count) = match &state.managed_state {
            ManagedHostState::HostReprovision {
                reprovision_state,
                retry_count,
            } => (reprovision_state, *retry_count),
            ManagedHostState::Ready => (
                &HostReprovisionState::CheckingFirmwareV2 {
                    firmware_type: None,
                    firmware_number: None,
                },
                0,
            ),
            ManagedHostState::Assigned { instance_state } => match &instance_state {
                InstanceState::HostReprovision { reprovision_state } => (reprovision_state, 0),
                InstanceState::Ready => (
                    &HostReprovisionState::CheckingFirmwareV2 {
                        firmware_type: None,
                        firmware_number: None,
                    },
                    0,
                ),
                _ => {
                    return Err(StateHandlerError::InvalidState(format!(
                        "Invalid state for calling handle_host_reprovision {:?}",
                        state.managed_state
                    )));
                }
            },
            _ => {
                return Err(StateHandlerError::InvalidState(format!(
                    "Invalid state for calling handle_host_reprovision {:?}",
                    state.managed_state
                )));
            }
        };

        if state
            .host_snapshot
            .host_reprovision_requested
            .as_ref()
            .is_some_and(|host_reprovision_requested| {
                host_reprovision_requested.request_reset.unwrap_or(false)
            })
        {
            tracing::info!(%machine_id, "Host firmware upgrade reset requested, returning to CheckingFirmwareRepeat");
            retry_count = 0;
            host_reprovision_state = &HostReprovisionState::CheckingFirmwareRepeatV2 {
                firmware_type: None,
                firmware_number: None,
            };
            state.managed_state = ManagedHostState::HostReprovision {
                reprovision_state: HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_type: None,
                    firmware_number: None,
                },
                retry_count: 0,
            };
            ctx.pending_db_writes
                .push(MachineWriteOp::ResetHostReprovisioningRequest {
                    machine_id: *machine_id,
                    clear_reset: true,
                });
        }

        if is_rack_level_reprovisioning(state)
            && matches!(
                host_reprovision_state,
                HostReprovisionState::CheckingFirmware
                    | HostReprovisionState::CheckingFirmwareRepeat
                    | HostReprovisionState::CheckingFirmwareV2 { .. }
                    | HostReprovisionState::CheckingFirmwareRepeatV2 { .. }
            )
        {
            tracing::info!(
                %machine_id,
                "Rack-level firmware upgrade bypassing legacy host firmware checks"
            );
            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::WaitingForRackFirmwareUpgrade,
                retry_count,
            )));
        }

        match host_reprovision_state {
            HostReprovisionState::WaitingForRackFirmwareUpgrade => {
                self.handle_waiting_for_rack_firmware_upgrade(state, ctx, scenario)
                    .await
            }
            HostReprovisionState::CheckingFirmware => {
                self.host_checking_fw(
                    &HostReprovisionState::CheckingFirmwareV2 {
                        firmware_type: None,
                        firmware_number: None,
                    },
                    state,
                    ctx,
                    original_state,
                    scenario,
                    false,
                )
                .await
            }
            HostReprovisionState::CheckingFirmwareRepeat => {
                self.host_checking_fw(
                    &HostReprovisionState::CheckingFirmwareRepeatV2 {
                        firmware_type: None,
                        firmware_number: None,
                    },
                    state,
                    ctx,
                    original_state,
                    scenario,
                    false,
                )
                .await
            }
            details @ HostReprovisionState::CheckingFirmwareV2 { .. } => {
                self.host_checking_fw(details, state, ctx, original_state, scenario, false)
                    .await
            }
            details @ HostReprovisionState::CheckingFirmwareRepeatV2 { .. } => {
                self.host_checking_fw(details, state, ctx, original_state, scenario, true)
                    .await
            }
            HostReprovisionState::WaitingForManualUpgrade { .. } => {
                self.waiting_for_manual_upgrade(state, scenario)
            }
            HostReprovisionState::WaitingForScript { .. } => {
                self.waiting_for_script(state, scenario)
            }
            HostReprovisionState::InitialReset { phase, last_time } => {
                self.pre_update_resets(
                    state,
                    ctx.services,
                    scenario,
                    Some(phase.clone()),
                    &Some(*last_time),
                )
                .await
            }
            details @ HostReprovisionState::WaitingForUpload { .. } => {
                self.waiting_for_upload(details, state, scenario, ctx).await
            }
            details @ HostReprovisionState::WaitingForFirmwareUpgrade { .. } => {
                self.host_waiting_fw(details, state, ctx, machine_id, scenario)
                    .await
            }
            details @ HostReprovisionState::ResetForNewFirmware { .. } => {
                self.host_reset_for_new_firmware(state, ctx, machine_id, details, scenario)
                    .await
            }
            details @ HostReprovisionState::NewFirmwareReportedWait { .. } => {
                self.host_new_firmware_reported_wait(state, ctx, details, machine_id, scenario)
                    .await
            }
            HostReprovisionState::WaitingForScoutUpgrade {
                firmware_type,
                final_version,
                power_drains_needed,
                deadline,
                result,
                ..
            } => {
                if let Some(result) = result {
                    if result.success {
                        tracing::info!(
                            machine_id = %state.host_snapshot.id,
                            "Scout firmware upgrade succeeded"
                        );
                        let next_reprov_state = HostReprovisionState::ResetForNewFirmware {
                            final_version: final_version.to_string(),
                            firmware_type: *firmware_type,
                            firmware_number: None,
                            power_drains_needed: *power_drains_needed,
                            delay_until: None,
                            last_power_drain_operation: None,
                            reset_retry_count: 0,
                        };
                        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                            next_reprov_state,
                            state.managed_state.get_host_repro_retry_count(),
                        )))
                    } else {
                        let reason = if result.error.is_empty() {
                            format!("scout upgrade failed with exit code {}", result.exit_code)
                        } else {
                            result.error.clone()
                        };
                        tracing::warn!(
                            machine_id = %state.host_snapshot.id,
                            reason = %reason,
                            "Scout firmware upgrade failed"
                        );
                        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                            HostReprovisionState::FailedFirmwareUpgrade {
                                firmware_type: *firmware_type,
                                report_time: Some(Utc::now()),
                                reason: Some(reason),
                            },
                            state.managed_state.get_host_repro_retry_count(),
                        )))
                    }
                } else if Utc::now() > *deadline {
                    tracing::warn!(
                        machine_id = %state.host_snapshot.id,
                        %deadline,
                        "Scout firmware upgrade timed out",
                    );
                    Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                        HostReprovisionState::FailedFirmwareUpgrade {
                            firmware_type: *firmware_type,
                            report_time: Some(Utc::now()),
                            reason: Some(format!(
                                "scout firmware upgrade timed out (deadline {deadline})"
                            )),
                        },
                        state.managed_state.get_host_repro_retry_count(),
                    )))
                } else {
                    Ok(StateHandlerOutcome::do_nothing())
                }
            }
            HostReprovisionState::FailedFirmwareUpgrade {
                report_time,
                reason,
                ..
            } => {
                // A special case in Rackfirmware upgrade to handle FailedFirmwareUpgrade
                // Accept a freshly-issued Host Reprovision request that arrives while we are
                // sitting in FailedFirmwareUpgrade. `trigger_host_reprovisioning_request`
                // overwrites `host_reprovisioning_requested` with `started_at = None`, so a
                // `None` here (after a previous failure) indicates a brand-new user request.
                // Mirror the ManagedHostState::Ready handling: route rack-level requests to
                // WaitingForRackFirmwareUpgrade, otherwise restart the host upgrade flow from
                // CheckingFirmwareV2 (retry_count reset to 0) and merge the host-fw health
                // report alert.
                if state
                    .host_snapshot
                    .host_reprovision_requested
                    .as_ref()
                    .is_some_and(|req| req.started_at.is_none())
                    && is_rack_level_reprovisioning(state)
                {
                    tracing::info!(
                        %machine_id,
                        "Rack-level firmware upgrade requested in FailedFirmwareUpgrade — entering WaitingForRackFirmwareUpgrade",
                    );
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::HostReprovision {
                            retry_count: 1,
                            reprovision_state: HostReprovisionState::WaitingForRackFirmwareUpgrade,
                        },
                    ));
                }

                let can_retry = retry_count < MAX_FIRMWARE_UPGRADE_RETRIES;
                let waited_enough = Utc::now()
                    .signed_duration_since(report_time.unwrap_or(Utc::now()))
                    >= ctx
                        .services
                        .site_config
                        .firmware_global
                        .host_firmware_upgrade_retry_interval;

                if can_retry && waited_enough {
                    carbide_instrument::emit(HostFirmwareUpgradeRetried {
                        machine_id: *machine_id,
                        attempt: retry_count + 1,
                        error: reason.clone().unwrap_or_default(),
                    });

                    let reprovision_state = HostReprovisionState::CheckingFirmwareV2 {
                        firmware_type: None,
                        firmware_number: None,
                    };
                    Ok(StateHandlerOutcome::transition(
                        scenario.actual_new_state(reprovision_state, retry_count + 1),
                    ))
                } else if can_retry {
                    // Still inside the retry interval; a later pass decides.
                    Ok(StateHandlerOutcome::do_nothing())
                } else {
                    // No retry budget left; remain in this failure state until
                    // an operator intervenes. Exhausted machines are counted
                    // by the carbide_exhausted_reprovision_retry_count gauge,
                    // so nothing is logged per pass.
                    Ok(StateHandlerOutcome::do_nothing())
                }
            }
        }
    }

    /// Handles the WaitingForRackFirmwareUpgrade sub-state.
    /// The rack controller sets `ended_at` on `rack_fw_details` once the rack-level
    /// firmware upgrade finishes. When `ended_at` is set (and belongs to the current
    /// upgrade cycle), transition back to Ready and clear the reprovisioning request.
    async fn handle_waiting_for_rack_firmware_upgrade(
        &self,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let machine_id = state.host_snapshot.id;
        let requested_at = state
            .host_snapshot
            .host_reprovision_requested
            .as_ref()
            .map(|request| request.requested_at)
            .expect("WaitingForRackFirmwareUpgrade requires a rack reprovision request");
        let Some(rack_fw_status) = state.host_snapshot.rack_fw_details.as_ref() else {
            return Ok(StateHandlerOutcome::wait(
                "waiting for rack firmware status".into(),
            ));
        };
        if !rack_fw_status.is_current_for(requested_at) {
            return Ok(StateHandlerOutcome::wait(
                "waiting for current rack firmware cycle".into(),
            ));
        }
        if !rack_fw_status.is_terminal() {
            return Ok(StateHandlerOutcome::wait(
                "waiting for rack firmware completion".into(),
            ));
        }

        let next_state = match &rack_fw_status.status {
            model::rack::RackFirmwareUpgradeState::Completed => scenario.complete_state(),
            model::rack::RackFirmwareUpgradeState::Failed { cause } => scenario.actual_new_state(
                HostReprovisionState::FailedFirmwareUpgrade {
                    firmware_type: FirmwareComponentType::Unknown,
                    report_time: Some(Utc::now()),
                    reason: Some(cause.clone()),
                },
                state.managed_state.get_host_repro_retry_count(),
            ),
            model::rack::RackFirmwareUpgradeState::Started
            | model::rack::RackFirmwareUpgradeState::InProgress => {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for rack firmware completion".into(),
                ));
            }
        };

        Ok(StateHandlerOutcome::transition(next_state)
            .in_transaction(&ctx.services.db_pool, move |txn| {
                async move {
                    db::host_machine_update::clear_host_reprovisioning_request(txn, &machine_id)
                        .await?;
                    Ok::<_, DatabaseError>(())
                }
                .boxed()
            })
            .await??)
    }

    #[allow(clippy::too_many_arguments)]
    async fn host_checking_fw(
        &self,
        details: &HostReprovisionState,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        original_state: &ManagedHostState,
        scenario: HostFirmwareScenario,
        repeat: bool,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let machine_id = state.host_snapshot.id;
        let ret = self
            .host_checking_fw_noclear(details, state, ctx, &machine_id, scenario, repeat)
            .await?;

        // Check if we are returning to the ready state, and clear the host reprovisioning request if so.
        let mut ret = match ret {
            StateHandlerOutcome::Transition {
                next_state:
                    ManagedHostState::HostReprovision { .. }
                    | ManagedHostState::Assigned {
                        instance_state: InstanceState::HostReprovision { .. },
                    },
                ..
            } => ret,
            _ => {
                ret.in_transaction(&ctx.services.db_pool, move |txn| {
                    async move {
                        db::host_machine_update::clear_host_reprovisioning_request(
                            txn,
                            &machine_id,
                        )
                        .await?;
                        // TODO: Remove when manual upgrade feature is removed
                        db::host_machine_update::clear_manual_firmware_upgrade_completed(
                            txn,
                            &machine_id,
                        )
                        .await?;
                        Ok::<_, DatabaseError>(())
                    }
                    .boxed()
                })
                .await??
            }
        };

        if let StateHandlerOutcome::Transition { next_state, .. } = &ret
            && next_state == original_state
        {
            // host_checking_fw_noclear can return Ready to indicate that we're moving out of CheckingFirmware,
            // but we also take this path when we're actually in Ready - for that case, return do_nothing() so that
            // we don't keep retransitioning to the same state.
            return Ok(StateHandlerOutcome::do_nothing().with_txn_opt(ret.take_transaction()));
        }

        Ok(ret)
    }

    #[allow(clippy::too_many_arguments)]
    async fn host_checking_fw_noclear(
        &self,
        details: &HostReprovisionState,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        machine_id: &MachineId,
        scenario: HostFirmwareScenario,
        repeat: bool,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        // temporary check if manual upgrade is required before proceeding with automatic ones,
        // should be removed once we complete upgrades through the scout.
        // For now, only gb200s need manual upgrades.
        if requires_manual_firmware_upgrade(state, &ctx.services.site_config.firmware_global) {
            tracing::info!(
                %machine_id,
                "Machine (GB200) requires manual firmware upgrade, transitioning to WaitingForManualUpgrade"
            );
            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::WaitingForManualUpgrade {
                    manual_upgrade_started: Utc::now(),
                },
                state.managed_state.get_host_repro_retry_count(),
            )));
        }

        let (current_firmware_type, current_firmware_number): (Option<FirmwareComponentType>, u32) =
            match details {
                HostReprovisionState::CheckingFirmwareV2 {
                    firmware_number,
                    firmware_type,
                }
                | HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_number,
                    firmware_type,
                } => (*firmware_type, firmware_number.unwrap_or(0)),
                _ => {
                    return Err(StateHandlerError::GenericError(eyre!(
                        "wrong enum in host_checking_fw_noclear"
                    )));
                }
            };

        let Some(explored_endpoint) =
            find_explored_refreshed_endpoint(state, machine_id, ctx).await?
        else {
            // find_explored_refreshed_endpoint's behavior is to return None to indicate we're waiting for an update, not to indicate there isn't anything.

            tracing::debug!(%machine_id, "Managed host waiting for site explorer to revisit");
            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_type: current_firmware_type,
                    firmware_number: Some(current_firmware_number),
                },
                state.managed_state.get_host_repro_retry_count(),
            )));
        };

        let fw_config_snapshot = self.host_firmware_config_snapshot(ctx).await?;
        let Some(fw_info) = fw_config_snapshot.find_fw_info_for_host(&explored_endpoint) else {
            return Ok(StateHandlerOutcome::transition(scenario.complete_state()));
        };

        for firmware_type in fw_info.ordering() {
            // ordering() will give a list of firmware types in the order they should be installed.
            // So, `firmware_type` may not be equal to `current_firmware_type` inside this loop.
            // We need to set `firmware_number` to 0 in case they are not equal because `firmware_number` coming
            // from outside this loop belongs only to the `current_firmware_type`
            let firmware_number = if let Some(ft) = current_firmware_type
                && ft == firmware_type
            {
                current_firmware_number
            } else {
                0
            };

            if let Some(to_install) =
                need_host_fw_upgrade(&explored_endpoint, &fw_info, firmware_type)
            {
                let scout_script = crate::scout_firmware_scripts::find_scout_script(
                    &ctx.services.site_config.pxe_public_base_url,
                    fw_info.vendor,
                    &fw_info.model,
                    firmware_type,
                )
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre!(
                        "failed to resolve scout firmware script for vendor={}, model={}, component={}: {error}",
                        fw_info.vendor,
                        fw_info.model,
                        firmware_type
                    ))
                })?;

                if let Some(scout_script) = scout_script {
                    let firmware_dir = &ctx.services.site_config.firmware_global.firmware_directory;
                    let pxe_public_base_url = ctx
                        .services
                        .site_config
                        .pxe_public_base_url
                        .trim_end_matches('/');

                    let upgrade_task_id = uuid::Uuid::new_v4().to_string();
                    let file_artifact_count = to_install.files.len();
                    let task = ScoutFirmwareUpgradeTask {
                        upgrade_task_id: upgrade_task_id.clone(),
                        component_type: firmware_type.to_string(),
                        target_version: to_install.version.clone(),
                        script: Some(FileArtifact {
                            url: scout_script.url,
                            sha256: scout_script.sha256,
                        }),
                        execution_timeout_seconds: scout_script.execution_timeout_seconds,
                        artifact_download_timeout_seconds: scout_script
                            .artifact_download_timeout_seconds,
                        file_artifacts: to_install
                            .files
                            .iter()
                            .map(|artifact| {
                                firmware_artifact::resolve_scout_file_artifact(
                                    pxe_public_base_url,
                                    firmware_dir,
                                    artifact,
                                )
                            })
                            .collect::<Result<Vec<_>, StateHandlerError>>()?,
                    };

                    // Scout uses a fixed timeout for the script download and applies the artifact
                    // download timeout per file
                    let started_at = Utc::now();
                    let deadline = scout_firmware_upgrade_deadline(
                        started_at,
                        scout_script.execution_timeout_seconds,
                        scout_script.artifact_download_timeout_seconds,
                        file_artifact_count,
                    );
                    return Ok(StateHandlerOutcome::transition(
                        scenario.actual_new_state(
                            HostReprovisionState::WaitingForScoutUpgrade {
                                upgrade_task_id,
                                firmware_type,
                                final_version: to_install.version,
                                power_drains_needed: to_install.power_drains_needed,
                                started_at,
                                deadline,
                                // Safety: The #[derive(Serialize)] impl does not fail
                                task_json: serde_json::to_string(&task)
                                    .expect("BUG: derived Serialize impl failed?"),
                                result: None,
                            },
                            state.managed_state.get_host_repro_retry_count(),
                        ),
                    ));
                }

                if to_install.script.is_some() {
                    return self
                        .by_script(to_install, state, explored_endpoint, scenario)
                        .await;
                }

                tracing::info!(
                    %machine_id,
                    firmware = ?to_install,
                    firmware_number,
                    bmc_ip_address = %explored_endpoint.address,
                    "Installing firmware"
                );

                if !repeat && to_install.pre_update_resets {
                    return self
                        .pre_update_resets(state, ctx.services, scenario, None, &None)
                        .await;
                }

                return self
                    .initiate_host_fw_update(
                        explored_endpoint.address,
                        state,
                        ctx,
                        FullFirmwareInfo {
                            model: fw_info.model.as_str(),
                            to_install: &to_install,
                            component_type: &firmware_type,
                            firmware_number: &firmware_number,
                        },
                        scenario,
                    )
                    .await;
            }
        }

        // Nothing needs updates, return to ready.  But first, we may need to reenable lockdown.

        let redfish_client = ctx
            .services
            .create_redfish_client_from_machine(&state.host_snapshot)
            .await?;

        let lockdown_disabled = match redfish_client.lockdown_status().await {
            Ok(status) => !status.is_fully_enabled(), // If it was partial, treat as disabled so we will fully enable it
            Err(e) => {
                if let libredfish::RedfishError::NotSupported(_) = e {
                    // Returned when the platform doesn't support lockdown, so here we say it's not disabled
                    // Note that this is different from the place where we do something similar
                    false
                } else {
                    tracing::warn!(
                        %machine_id,
                        error = %e,
                        "Could not get lockdown status"
                    );
                    return Ok(StateHandlerOutcome::do_nothing());
                }
            }
        };

        if lockdown_disabled && !state.host_snapshot.host_profile.disable_lockdown {
            tracing::debug!("host firmware update: Reenabling lockdown");
            if let Err(e) = redfish_client
                .lockdown(libredfish::EnabledDisabled::Enabled)
                .await
            {
                tracing::error!(%machine_id, error = %e, "Could not set lockdown");
                return Ok(StateHandlerOutcome::do_nothing());
            }
            match scenario {
                HostFirmwareScenario::Ready => {
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::HostInit {
                            machine_state: MachineState::WaitingForLockdown {
                                lockdown_info: LockdownInfo {
                                    state: LockdownState::PollingLockdownStatus,
                                    mode: Enable,
                                },
                            },
                        },
                    ));
                }
                HostFirmwareScenario::Instance => {
                    handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart)
                        .await?;
                    return Ok(StateHandlerOutcome::transition(scenario.complete_state()));
                }
            }
        }

        if lockdown_disabled && state.host_snapshot.host_profile.disable_lockdown {
            tracing::info!(
                %machine_id,
                "host firmware update: host is NOT locked down -- skipping lockdown re-enable per expected-machine config"
            );
        } else if !lockdown_disabled {
            tracing::debug!(
                "host firmware update: Don't need to reenable lockdown -- host is already locked down"
            );
        }

        if let HostFirmwareScenario::Instance = scenario {
            handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart).await?;
        }
        Ok(StateHandlerOutcome::transition(scenario.complete_state()))
    }

    async fn by_script(
        &self,
        to_install: FirmwareEntry,
        state: &ManagedHostStateSnapshot,
        explored_endpoint: ExploredEndpoint,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let machine_id = state.host_snapshot.id;

        self.upgrade_script_state.started(machine_id.to_string());

        let address = explored_endpoint.address.to_string().clone();
        let script = to_install.script.unwrap_or("/bin/false".into()); // Should always be Some at this point
        let upgrade_script_state = self.upgrade_script_state.clone();
        let (username, password) = if let Some(credential_reader) = &self.credential_reader {
            let bmc_mac_address = state.host_snapshot.status.bmc_info.mac.ok_or_else(|| {
                StateHandlerError::MissingData {
                    object_id: state.host_snapshot.id.to_string(),
                    missing: "bmc_mac",
                }
            })?;
            let key = CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
            };
            match credential_reader.get_credentials(&key).await {
                Ok(Some(credentials)) => match credentials {
                    Credentials::UsernamePassword { username, password } => (username, password),
                },
                Ok(None) => {
                    return Err(StateHandlerError::GenericError(eyre!(
                        "no BMC credentials exists"
                    )));
                }
                Err(e) => {
                    return Err(StateHandlerError::GenericError(eyre!(
                        "unable to get BMC credentials: {e}"
                    )));
                }
            }
        } else {
            ("Unknown".to_string(), "Unknown".to_string())
        };
        tokio::spawn(async move {
            let mut cmd = match tokio::process::Command::new(script)
                .env("BMC_IP", address.clone())
                .env("BMC_USERNAME", username)
                .env("BMC_PASSWORD", password)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(cmd) => cmd,
                Err(e) => {
                    tracing::error!(
                        %machine_id,
                        bmc_ip_address = %address,
                        error = %e,
                        "Upgrade script command creation failed"
                    );
                    upgrade_script_state.completed(machine_id.to_string(), false);
                    return;
                }
            };

            let Some(stdout) = cmd.stdout.take() else {
                tracing::error!(
                    %machine_id,
                    bmc_ip_address = %address,
                    "Upgrade script STDOUT creation failed"
                );
                let _ = cmd.kill().await;
                let _ = cmd.wait().await;
                upgrade_script_state.completed(machine_id.to_string(), false);
                return;
            };
            let stdout = tokio::io::BufReader::new(stdout);

            let Some(stderr) = cmd.stderr.take() else {
                tracing::error!(
                    %machine_id,
                    bmc_ip_address = %address,
                    "Upgrade script STDERR creation failed"
                );
                let _ = cmd.kill().await;
                let _ = cmd.wait().await;
                upgrade_script_state.completed(machine_id.to_string(), false);
                return;
            };
            let stderr = tokio::io::BufReader::new(stderr);

            // Record the script streams with searchable fields.
            let stderr_machine_id = machine_id;
            let stderr_address = address.clone();
            let thread = tokio::spawn(async move {
                let mut lines = stderr.lines();
                while let Some(line) = lines.next_line().await.unwrap_or(None) {
                    tracing::info!(
                        machine_id = %stderr_machine_id,
                        bmc_ip_address = %stderr_address,
                        stream = "stderr",
                        output = %line,
                        "Upgrade script output"
                    );
                }
            });
            let mut lines = stdout.lines();
            while let Some(line) = lines.next_line().await.unwrap_or(None) {
                tracing::info!(
                    %machine_id,
                    bmc_ip_address = %address,
                    stream = "stdout",
                    output = %line,
                    "Upgrade script output"
                );
            }
            let _ = tokio::join!(thread);

            match cmd.wait().await {
                Err(e) => {
                    tracing::info!(
                        %machine_id,
                        bmc_ip_address = %address,
                        error = %e,
                        "Upgrade script FAILED: Wait failure"
                    );
                    upgrade_script_state.completed(machine_id.to_string(), false);
                }
                Ok(errorcode) => {
                    if errorcode.success() {
                        tracing::info!(
                            %machine_id,
                            bmc_ip_address = %address,
                            "Upgrade script completed successfully"
                        );
                        upgrade_script_state.completed(machine_id.to_string(), true);
                    } else {
                        tracing::warn!(
                            %machine_id,
                            bmc_ip_address = %address,
                            exit_status = %errorcode,
                            "Upgrade script FAILED: Exited"
                        );
                        upgrade_script_state.completed(machine_id.to_string(), false);
                    }
                }
            }
        });

        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
            HostReprovisionState::WaitingForScript {},
            state.managed_state.get_host_repro_retry_count(),
        )))
    }

    fn waiting_for_manual_upgrade(
        &self,
        state: &ManagedHostStateSnapshot,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let machine_id = &state.host_snapshot.id;

        if let Some(completed_at) = state.host_snapshot.manual_firmware_upgrade_completed {
            tracing::info!(
                %machine_id,
                %completed_at,
                "Manual firmware upgrade completed, proceeding to automatic upgrades"
            );

            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_type: None,
                    firmware_number: None,
                },
                state.managed_state.get_host_repro_retry_count(),
            )));
        }

        tracing::debug!(
            %machine_id,
            "Machine still waiting for manual firmware upgrade to be marked complete"
        );
        Ok(StateHandlerOutcome::do_nothing())
    }

    fn waiting_for_script(
        &self,
        state: &ManagedHostStateSnapshot,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let machine_id = state.host_snapshot.id.to_string();
        let Some(success) = self.upgrade_script_state.state(&machine_id) else {
            // Not yet completed, or we restarted (which specifically needs a manual restart of interrupted scripts)
            return Ok(StateHandlerOutcome::do_nothing());
        };

        self.upgrade_script_state.clear(&machine_id);

        if success {
            Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_type: None,
                    firmware_number: None,
                },
                state.managed_state.get_host_repro_retry_count(),
            )))
        } else {
            let reprovision_state = HostReprovisionState::FailedFirmwareUpgrade {
                firmware_type: FirmwareComponentType::Unknown,
                report_time: Some(Utc::now()),
                reason: Some(format!(
                    "The upgrade script failed. Search logs for the \"Upgrade script output\" message with machine_id={} to find script output. Use \"forge-admin-cli mh reset-host-reprovisioning --machine {}\" to retry.",
                    state.host_snapshot.id, state.host_snapshot.id
                )),
            };
            Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                reprovision_state,
                state.managed_state.get_host_repro_retry_count(),
            )))
        }
    }

    async fn pre_update_resets(
        &self,
        state: &ManagedHostStateSnapshot,
        services: &MachineStateHandlerServices,
        scenario: HostFirmwareScenario,
        phase: Option<InitialResetPhase>,
        last_time: &Option<DateTime<Utc>>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let redfish_client = services
            .create_redfish_client_from_machine(&state.host_snapshot)
            .await?;

        match phase.unwrap_or(InitialResetPhase::Start) {
            InitialResetPhase::Start => {
                redfish_client
                    .power(SystemPowerControl::ForceOff)
                    .await
                    .map_err(|e| redfish_error("power off", e))?;
                let status = get_power_state(redfish_client.as_ref()).await?;
                if status != PowerState::Off {
                    return Err(StateHandlerError::GenericError(eyre!(
                        "host {} did not turn off when requested",
                        state.host_snapshot.id
                    )));
                }
                redfish_client
                    .bmc_reset(None)
                    .await
                    .map_err(|e| redfish_error("BMC reset", e))?;

                Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                    HostReprovisionState::InitialReset {
                        phase: InitialResetPhase::BMCWasReset,
                        last_time: Utc::now(),
                    },
                    state.managed_state.get_host_repro_retry_count(),
                )))
            }
            InitialResetPhase::BMCWasReset => {
                if let Err(_e) = redfish_client.get_tasks().await {
                    // BMC not fully up yet
                    return Ok(StateHandlerOutcome::do_nothing());
                }
                redfish_client
                    .power(SystemPowerControl::On)
                    .await
                    .map_err(|e| redfish_error("power on", e))?;
                let status = get_power_state(redfish_client.as_ref()).await?;
                if status != PowerState::On {
                    return Err(StateHandlerError::GenericError(eyre!(
                        "host {} did not turn on when requested",
                        state.host_snapshot.id
                    )));
                }
                Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                    HostReprovisionState::InitialReset {
                        phase: InitialResetPhase::WaitHostBoot,
                        last_time: Utc::now(),
                    },
                    state.managed_state.get_host_repro_retry_count(),
                )))
            }
            InitialResetPhase::WaitHostBoot => {
                if Utc::now().signed_duration_since(last_time.unwrap_or(Utc::now()))
                    < chrono::TimeDelta::minutes(20)
                {
                    // Wait longer
                    return Ok(StateHandlerOutcome::do_nothing());
                }
                // Now we can actually proceed with the upgrade.  Go back to checking firmware so we don't have to store all of that info.
                Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                    HostReprovisionState::CheckingFirmwareRepeatV2 {
                        firmware_type: None,
                        firmware_number: None,
                    },
                    state.managed_state.get_host_repro_retry_count(),
                )))
            }
        }
    }
    /// Uploads a firmware update via multipart, returning the task ID, or None if upload was deferred
    async fn initiate_host_fw_update(
        &self,
        address: IpAddr,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        fw_info: FullFirmwareInfo<'_>,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let snapshot = &state.host_snapshot;
        let to_install = fw_info.to_install;
        let component_type = fw_info.component_type;
        let artifact = firmware_artifact::resolve_firmware_artifact(
            &ctx.services
                .site_config
                .firmware_global
                .firmware_download_cache_directory,
            to_install,
            *fw_info.firmware_number,
        )?;

        match &artifact.source {
            ResolvedFirmwareArtifactSource::Remote { url, sha256 } => {
                if !self.downloader.available(&artifact.local_path, url, sha256) {
                    tracing::debug!(
                        artifact_path = %artifact.local_path.display(),
                        %url,
                        "firmware artifact is being downloaded, update deferred"
                    );

                    return Ok(StateHandlerOutcome::do_nothing());
                }
            }
            ResolvedFirmwareArtifactSource::Local => {
                if !artifact.local_path.exists() {
                    let reason = format!(
                        "firmware artifact {} for machine {} is not present and no firmware artifact URL is configured",
                        artifact.local_path.display(),
                        snapshot.id
                    );
                    tracing::warn!(
                        artifact_path = %artifact.local_path.display(),
                        machine_id = %snapshot.id,
                        "firmware artifact is not present and no firmware artifact URL is configured",
                    );
                    return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                        HostReprovisionState::FailedFirmwareUpgrade {
                            firmware_type: *component_type,
                            report_time: Some(Utc::now()),
                            reason: Some(reason),
                        },
                        state.managed_state.get_host_repro_retry_count(),
                    )));
                }
            }
        };

        let Ok(_active) = self.upload_limiter.try_acquire() else {
            tracing::debug!(
                firmware = ?to_install,
                machine_id = %snapshot.id,
                "Deferring installation, too many uploads already active",
            );
            return Ok(StateHandlerOutcome::do_nothing());
        };

        // Setup the Redfish connection
        let redfish_client = ctx
            .services
            .create_redfish_client_from_machine(snapshot)
            .await?;

        let lockdown_disabled = match redfish_client.lockdown_status().await {
            Ok(status) => status.is_fully_disabled(), // If we're partial, we want to act like it was enabled so we disable it
            Err(e) => {
                if let libredfish::RedfishError::NotSupported(_) = e {
                    // Returned when the platform doesn't support lockdown, so here we say it's already disabled
                    // Note that this is different from the place where we do something similar
                    true
                } else {
                    tracing::warn!(
                        machine_id = %state.host_snapshot.id,
                        error = %e,
                        "Could not get lockdown status"
                    );
                    return Ok(StateHandlerOutcome::do_nothing());
                }
            }
        };
        if lockdown_disabled {
            // Already disabled, we can go ahead
            tracing::debug!("Host fw update: No need for disabling lockdown");
        } else {
            tracing::info!(bmc_ip_address = %address, "Host fw update: Disabling lockdown");
            if let Err(e) = redfish_client
                .lockdown(libredfish::EnabledDisabled::Disabled)
                .await
            {
                tracing::warn!(bmc_ip_address = %address, error = %e, "Could not set lockdown");
                return Ok(StateHandlerOutcome::do_nothing());
            }
            if fw_info.model == "Dell" {
                tracing::info!(bmc_ip_address = %address, "Host fw update: Rebooting after disabling lockdown because Dell");
                handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart).await?;
                // Wait until the next state machine iteration to let it restart
                return Ok(StateHandlerOutcome::do_nothing());
            }
        }

        let machine_id = state.host_snapshot.id.to_string();
        let filename = artifact.local_path;
        let redfish_component_type: libredfish::model::update_service::ComponentType =
            match to_install.install_only_specified {
                false => libredfish::model::update_service::ComponentType::Unknown,
                true => component_type.into_libredfish(),
            };
        let address = address.to_string();

        self.async_firmware_uploader.start_upload(
            machine_id,
            redfish_client,
            filename,
            redfish_component_type,
            address,
        );

        // Upload complete and updated started, will monitor task in future iterations
        let reprovision_state = HostReprovisionState::WaitingForUpload {
            firmware_type: *fw_info.component_type,
            final_version: fw_info.to_install.version.clone(),
            power_drains_needed: fw_info.to_install.power_drains_needed,
            firmware_number: Some(*fw_info.firmware_number),
        };

        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
            reprovision_state,
            state.managed_state.get_host_repro_retry_count(),
        )))
    }

    async fn waiting_for_upload(
        &self,
        details: &HostReprovisionState,
        state: &ManagedHostStateSnapshot,
        scenario: HostFirmwareScenario,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let (final_version, firmware_type, power_drains_needed, firmware_number) = match details {
            HostReprovisionState::WaitingForUpload {
                final_version,
                firmware_type,
                power_drains_needed,
                firmware_number,
            } => (
                final_version,
                firmware_type,
                power_drains_needed,
                firmware_number,
            ),
            _ => {
                return Err(StateHandlerError::GenericError(eyre!(
                    "wrong enum in waiting_for_upload"
                )));
            }
        };

        let machine_id = state.host_snapshot.id;
        let address = match find_explored_refreshed_endpoint(state, &machine_id, ctx).await {
            Ok(explored_endpoint) => match explored_endpoint {
                Some(explored_endpoint) => explored_endpoint.address.to_string(),
                None => "unknown".to_string(),
            },
            Err(_) => "unknown".to_string(),
        };
        let machine_id = machine_id.to_string();
        match self.async_firmware_uploader.upload_status(&machine_id) {
            None => {
                tracing::info!(
                    %machine_id,
                    bmc_ip_address = %address,
                    "Apparent restart before upload completion, returning to CheckingFirmware"
                );
                Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                    HostReprovisionState::CheckingFirmwareRepeatV2 {
                        firmware_type: Some(*firmware_type),
                        firmware_number: *firmware_number,
                    },
                    state.managed_state.get_host_repro_retry_count(),
                )))
            }
            Some(upload_status) => {
                match upload_status {
                    None => {
                        tracing::debug!(
                            %machine_id,
                            bmc_ip_address = %address,
                            "Upload not yet complete"
                        );
                        Ok(StateHandlerOutcome::do_nothing())
                    }
                    Some(result) => {
                        match result {
                            UploadResult::Success { task_id } => {
                                // We want to remove the machine ID from the hashmap, but do not do it here, because we may fail the commit.  Run it in the next state handling.  Failure case doesn't matter, it would have identical behavior.
                                tracing::info!(
                                    %machine_id,
                                    bmc_ip_address = %address,
                                    %task_id,
                                    "Upload completed"
                                );
                                // Upload complete and updated started, will monitor task in future iterations
                                let reprovision_state =
                                    HostReprovisionState::WaitingForFirmwareUpgrade {
                                        task_id,
                                        firmware_type: *firmware_type,
                                        final_version: final_version.clone(),
                                        power_drains_needed: *power_drains_needed,
                                        firmware_number: *firmware_number,
                                        started_waiting: Some(Utc::now()),
                                    };
                                Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                                    reprovision_state,
                                    state.managed_state.get_host_repro_retry_count(),
                                )))
                            }
                            UploadResult::Failure => {
                                self.async_firmware_uploader.finish_upload(&machine_id);
                                // The upload thread already logged this
                                Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                                    HostReprovisionState::CheckingFirmwareRepeatV2 {
                                        firmware_type: Some(*firmware_type),
                                        firmware_number: *firmware_number,
                                    },
                                    state.managed_state.get_host_repro_retry_count(),
                                )))
                            }
                        }
                    }
                }
            }
        }
    }

    async fn host_waiting_fw(
        &self,
        details: &HostReprovisionState,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        machine_id: &MachineId,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let (
            task_id,
            final_version,
            firmware_type,
            power_drains_needed,
            firmware_number,
            started_waiting,
        ) = match details {
            HostReprovisionState::WaitingForFirmwareUpgrade {
                task_id,
                final_version,
                firmware_type,
                power_drains_needed,
                firmware_number,
                started_waiting,
            } => (
                task_id,
                final_version,
                firmware_type,
                power_drains_needed,
                firmware_number,
                started_waiting,
            ),
            _ => {
                return Err(StateHandlerError::GenericError(eyre!(
                    "wrong enum in host_waiting_fw"
                )));
            }
        };

        // Now it's safe to clear the hashmap for the upload status
        self.async_firmware_uploader
            .finish_upload(&state.host_snapshot.id.to_string());

        let address = state
            .host_snapshot
            .status
            .bmc_info
            .ip_addr()
            .map_err(StateHandlerError::GenericError)?;
        // Setup the Redfish connection
        let redfish_client = ctx
            .services
            .create_redfish_client_from_machine(&state.host_snapshot)
            .await?;

        match redfish_client.get_task(task_id.as_str()).await {
            Ok(task_info) => {
                match task_info.task_state {
                    Some(TaskState::New)
                    | Some(TaskState::Starting)
                    | Some(TaskState::Running)
                    | Some(TaskState::Pending) => {
                        tracing::debug!(
                            %machine_id,
                            task_state = ?task_info.task_state,
                            messages = ?task_info.messages,
                            "Upgrade task not yet complete",
                        );
                        Ok(StateHandlerOutcome::do_nothing())
                    }
                    Some(TaskState::Completed) => {
                        // Task has completed, update is done and we can clean up.  Site explorer will ingest this next time it runs on this endpoint.

                        // If we have multiple firmware files to be uploaded, do the next one.
                        if let Some(endpoint) =
                            find_explored_refreshed_endpoint(state, machine_id, ctx).await?
                        {
                            let fw_config_snapshot =
                                self.host_firmware_config_snapshot(ctx).await?;
                            let selected_firmware = fw_config_snapshot
                                .find_fw_info_for_host(&endpoint)
                                .and_then(|fw_info| {
                                    fw_info.components.get(firmware_type).and_then(
                                        |component_info| {
                                            component_info
                                                .known_firmware
                                                .iter()
                                                .find(|&x| x.version == final_version.as_str())
                                                .or_else(|| {
                                                    component_info
                                                        .known_firmware
                                                        .iter()
                                                        .find(|&x| x.default)
                                                })
                                                .cloned()
                                        },
                                    )
                                });

                            if let Some(selected_firmware) = selected_firmware {
                                let firmware_number = firmware_number.unwrap_or(0) + 1;
                                let has_more_artifacts = usize::try_from(firmware_number)
                                    .map(|firmware_number| {
                                        firmware_number < selected_firmware.artifact_count()
                                    })
                                    .unwrap_or(false);
                                if has_more_artifacts {
                                    tracing::debug!(
                                        %machine_id,
                                        firmware = ?selected_firmware,
                                        firmware_number,
                                        bmc_ip_address = %endpoint.address,
                                        "Moving firmware chain step to CheckingFirmware"
                                    );

                                    // There are more files to install.
                                    // Move to CheckingFirmware and start installing
                                    let reprovision_state =
                                        HostReprovisionState::CheckingFirmwareV2 {
                                            firmware_type: Some(*firmware_type),
                                            firmware_number: Some(firmware_number),
                                        };

                                    return Ok(StateHandlerOutcome::transition(
                                        scenario.actual_new_state(
                                            reprovision_state,
                                            state.managed_state.get_host_repro_retry_count(),
                                        ),
                                    ));
                                }
                            }
                        }

                        tracing::debug!(
                            %machine_id,
                            "Saw completion of host firmware upgrade task"
                        );

                        let reprovision_state = HostReprovisionState::ResetForNewFirmware {
                            final_version: final_version.to_string(),
                            firmware_type: *firmware_type,
                            firmware_number: *firmware_number,
                            power_drains_needed: *power_drains_needed,
                            delay_until: None,
                            last_power_drain_operation: None,
                            reset_retry_count: 0,
                        };
                        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                            reprovision_state,
                            state.managed_state.get_host_repro_retry_count(),
                        )))
                    }
                    Some(TaskState::Exception)
                    | Some(TaskState::Interrupted)
                    | Some(TaskState::Killed)
                    | Some(TaskState::Cancelled) => {
                        let msg = format!(
                            "Failure in firmware upgrade for {}: {} {:?}",
                            machine_id,
                            task_info.task_state.unwrap(),
                            task_info
                                .messages
                                .last()
                                .map_or("".to_string(), |m| m.message.clone())
                        );
                        tracing::warn!(
                            %machine_id,
                            reason = %msg,
                            "Firmware upgrade task failed",
                        );

                        // We need site explorer to requery the version, just in case it actually did get done
                        let mut txn = ctx.services.db_pool.begin().await?;

                        db::explored_endpoints::set_waiting_for_explorer_refresh(address, &mut txn)
                            .await?;

                        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                            HostReprovisionState::FailedFirmwareUpgrade {
                                firmware_type: *firmware_type,
                                report_time: Some(Utc::now()),
                                reason: Some(msg),
                            },
                            state.managed_state.get_host_repro_retry_count(),
                        ))
                        .with_txn(txn))
                    }
                    _ => {
                        // Unexpected state
                        let msg = format!(
                            "Unrecognized task state for {}: {:?}",
                            machine_id, task_info.task_state
                        );
                        tracing::warn!(
                            %machine_id,
                            reason = %msg,
                            "Unrecognized firmware upgrade task state",
                        );

                        let reprovision_state = HostReprovisionState::FailedFirmwareUpgrade {
                            firmware_type: *firmware_type,
                            report_time: Some(Utc::now()),
                            reason: Some(msg),
                        };
                        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                            reprovision_state,
                            state.managed_state.get_host_repro_retry_count(),
                        )))
                    }
                }
            }
            Err(e) => match e {
                RedfishError::HTTPErrorCode { status_code, .. } => {
                    if status_code == NOT_FOUND {
                        // Dells (maybe others) have been observed to not have report the job any more after completing a host reboot for a UEFI upgrade.  If we get a 404 but see that we're at the right version, we're done with that upgrade.
                        let Some(endpoint) =
                            find_explored_refreshed_endpoint(state, machine_id, ctx).await?
                        else {
                            return Ok(StateHandlerOutcome::do_nothing());
                        };

                        let fw_config_snapshot = self.host_firmware_config_snapshot(ctx).await?;
                        if let Some(fw_info) = fw_config_snapshot.find_fw_info_for_host(&endpoint)
                            && let Some(current_version) =
                                endpoint.find_version(&fw_info, *firmware_type)
                            && current_version == final_version
                        {
                            tracing::info!(
                                %machine_id,
                                bmc_ip_address = %endpoint.address,
                                "Marking completion of Redfish task of firmware upgrade with missing task"
                            );

                            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                                HostReprovisionState::CheckingFirmwareRepeatV2 {
                                    firmware_type: Some(*firmware_type),
                                    firmware_number: *firmware_number,
                                },
                                state.managed_state.get_host_repro_retry_count(),
                            )));
                        }

                        // We have also observed (FORGE-6177) the upgrade somehow disappearing, but working when retried.  If a long time has passed, go back to checking to retry.
                        if let Some(started_waiting) = started_waiting
                            && Utc::now().signed_duration_since(started_waiting)
                                > chrono::TimeDelta::minutes(15)
                        {
                            tracing::info!(
                                %machine_id,
                                bmc_ip_address = %endpoint.address,
                                "Timed out with missing Redfish task for firmware upgrade, returning to CheckingFirmware"
                            );
                            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                                HostReprovisionState::CheckingFirmwareRepeatV2 {
                                    firmware_type: Some(*firmware_type),
                                    firmware_number: *firmware_number,
                                },
                                state.managed_state.get_host_repro_retry_count(),
                            )));
                        }
                    }
                    Err(redfish_error("get_task", e))
                }
                _ => Err(redfish_error("get_task", e)),
            },
        }
    }

    async fn host_reset_for_new_firmware(
        &self,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        machine_id: &MachineId,
        details: &HostReprovisionState,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let (
            final_version,
            firmware_type,
            firmware_number,
            power_drains_needed,
            delay_until,
            last_power_drain_operation,
            reset_retry_count,
        ) = match details {
            HostReprovisionState::ResetForNewFirmware {
                final_version,
                firmware_type,
                firmware_number,
                power_drains_needed,
                delay_until,
                last_power_drain_operation,
                reset_retry_count,
            } => (
                final_version,
                firmware_type,
                firmware_number,
                power_drains_needed,
                delay_until,
                last_power_drain_operation,
                reset_retry_count,
            ),
            _ => {
                return Err(StateHandlerError::GenericError(eyre!(
                    "wrong enum in host_reset_for_new_firmware"
                )));
            }
        };

        let Some(endpoint) = find_explored_refreshed_endpoint(state, machine_id, ctx).await? else {
            tracing::debug!(%machine_id, "Waiting for site explorer to revisit");
            return Ok(StateHandlerOutcome::do_nothing());
        };

        if let Some(power_drains_needed) = power_drains_needed {
            if let Some(delay_until) = delay_until
                && *delay_until > chrono::Utc::now().timestamp()
            {
                tracing::info!(
                    last_power_drain_operation = ?last_power_drain_operation,
                    bmc_ip_address = %endpoint.address,
                    "Waiting after power drain operation"
                );
                return Ok(StateHandlerOutcome::do_nothing());
            }

            match last_power_drain_operation {
                None | Some(PowerDrainState::On) => {
                    // The 1000 is for unit tests; values above this will skip delays.
                    if *power_drains_needed == 0 || *power_drains_needed == 1000 {
                        tracing::info!(bmc_ip_address = %endpoint.address, "Power drains done");
                        // This path, and only this path of the match, exits the match and lets us proceed.  All others should return after updating state.
                    } else {
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            required_power_drain_count = *power_drains_needed,
                            "Upgrade task has completed but needs power drain(s), initiating one"
                        );
                        handler_host_power_control(state, ctx, SystemPowerControl::ForceOff)
                            .await?;

                        // Wait 60 seconds after powering off to do AC powercycle
                        let delay = if *power_drains_needed < 1000 { 60 } else { 0 };
                        let reprovision_state = HostReprovisionState::ResetForNewFirmware {
                            final_version: final_version.clone(),
                            firmware_type: *firmware_type,
                            firmware_number: *firmware_number,
                            power_drains_needed: Some(*power_drains_needed),
                            delay_until: Some(chrono::Utc::now().timestamp() + delay),
                            last_power_drain_operation: Some(PowerDrainState::Off),
                            reset_retry_count: *reset_retry_count,
                        };
                        return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                            reprovision_state,
                            state.managed_state.get_host_repro_retry_count(),
                        )));
                    }
                }
                Some(PowerDrainState::Off) => {
                    tracing::info!(bmc_ip_address = %endpoint.address, "Doing powercycle now");
                    handler_host_power_control(state, ctx, SystemPowerControl::ACPowercycle)
                        .await?;

                    let delay = if *power_drains_needed < 1000 { 90 } else { 0 };
                    let reprovision_state = HostReprovisionState::ResetForNewFirmware {
                        final_version: final_version.clone(),
                        firmware_type: *firmware_type,
                        firmware_number: *firmware_number,
                        power_drains_needed: Some(*power_drains_needed),
                        delay_until: Some(chrono::Utc::now().timestamp() + delay),
                        last_power_drain_operation: Some(PowerDrainState::Powercycle),
                        reset_retry_count: *reset_retry_count,
                    };
                    return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                        reprovision_state,
                        state.managed_state.get_host_repro_retry_count(),
                    )));
                }
                Some(PowerDrainState::Powercycle) => {
                    tracing::info!(bmc_ip_address = %endpoint.address, "Turning back on");
                    handler_host_power_control(state, ctx, SystemPowerControl::On).await?;

                    let delay = if *power_drains_needed < 1000 { 5 } else { 0 };
                    let reprovision_state = HostReprovisionState::ResetForNewFirmware {
                        final_version: final_version.clone(),
                        firmware_type: *firmware_type,
                        firmware_number: *firmware_number,
                        power_drains_needed: Some(power_drains_needed - 1),
                        delay_until: Some(chrono::Utc::now().timestamp() + delay),
                        last_power_drain_operation: Some(PowerDrainState::On),
                        reset_retry_count: *reset_retry_count,
                    };
                    return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                        reprovision_state,
                        state.managed_state.get_host_repro_retry_count(),
                    )));
                }
            };
        } else if firmware_type.is_uefi() {
            tracing::debug!(
                bmc_ip_address = %endpoint.address,
                "Upgrade task has completed but needs reboot, initiating one"
            );
            handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart).await?;

            // Same state but with the rebooted flag set, it can take a long time to reboot in some cases so we do not retry.
        }

        if firmware_type.is_bmc()
            && !endpoint
                .report
                .vendor
                .unwrap_or(bmc_vendor::BMCVendor::Unknown)
                .is_dell()
        {
            tracing::debug!(
                bmc_ip_address = %endpoint.address,
                "Upgrade task has completed but needs BMC reboot, initiating one"
            );
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;

            if let Err(e) = redfish_client.bmc_reset(None).await {
                tracing::warn!(bmc_ip_address = %endpoint.address, error = %e, "Failed to reboot");
                return Ok(StateHandlerOutcome::do_nothing());
            }
        }

        if (*firmware_type == FirmwareComponentType::HGXBmc
            || *firmware_type == FirmwareComponentType::Gpu)
            && !power_drains_needed.is_some()
        {
            // Needs a host power reset.  We might also have used the power drains to do an AC powercycle.
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;

            // We previously possibly tried to use ACPowerycle here, however that requires enough time for the BMC to come back.  We use
            // the power_drains_needed setting instead for that which is already aware of how to keep track of that sort of thing.
            if let Err(e) = redfish_client.power(SystemPowerControl::ForceOff).await {
                tracing::error!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Failed to power off"
                );
                return Ok(StateHandlerOutcome::do_nothing());
            }
            tokio::time::sleep(self.hgx_bmc_gpu_reboot_delay).await;
            if let Err(e) = redfish_client.power(SystemPowerControl::On).await {
                tracing::error!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Failed to power on"
                );
                return Ok(StateHandlerOutcome::do_nothing());
            }
            // Okay to proceed
        }

        // Now we can go on to waiting for the correct version to be reported
        let reprovision_state = HostReprovisionState::NewFirmwareReportedWait {
            firmware_type: *firmware_type,
            firmware_number: *firmware_number,
            final_version: final_version.to_string(),
            previous_reset_time: Some(Utc::now().timestamp()),
            reset_retry_count: *reset_retry_count,
        };
        Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
            reprovision_state,
            state.managed_state.get_host_repro_retry_count(),
        )))
    }

    async fn host_new_firmware_reported_wait(
        &self,
        state: &ManagedHostStateSnapshot,
        ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
        details: &HostReprovisionState,
        machine_id: &MachineId,
        scenario: HostFirmwareScenario,
    ) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
        let (final_version, firmware_type, firmware_number, previous_reset_time, reset_retry_count) =
            match details {
                HostReprovisionState::NewFirmwareReportedWait {
                    final_version,
                    firmware_type,
                    firmware_number,
                    previous_reset_time,
                    reset_retry_count,
                } => (
                    final_version,
                    firmware_type,
                    firmware_number,
                    previous_reset_time,
                    reset_retry_count,
                ),
                _ => {
                    return Err(StateHandlerError::GenericError(eyre!(
                        "wrong enum in host_new_firmware_reported_wait"
                    )));
                }
            };

        let Some(endpoint) = find_explored_refreshed_endpoint(state, machine_id, ctx).await? else {
            tracing::debug!(%machine_id, "Waiting for site explorer to revisit");
            return Ok(StateHandlerOutcome::do_nothing());
        };

        let fw_config_snapshot = self.host_firmware_config_snapshot(ctx).await?;
        let Some(fw_info) = fw_config_snapshot.find_fw_info_for_host(&endpoint) else {
            tracing::error!(%machine_id, "Could no longer find firmware info");
            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_type: Some(*firmware_type),
                    firmware_number: *firmware_number,
                },
                state.managed_state.get_host_repro_retry_count(),
            )));
        };

        let current_versions = endpoint.find_all_versions(&fw_info, *firmware_type);
        if current_versions.is_empty() {
            tracing::error!(%machine_id, "Could no longer find current versions");
            return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_type: Some(*firmware_type),
                    firmware_number: *firmware_number,
                },
                state.managed_state.get_host_repro_retry_count(),
            )));
        };

        let versions_match_final_version = current_versions.iter().all(|v| *v == final_version);
        if !versions_match_final_version {
            tracing::warn!(
                %machine_id,
                bmc_ip_address = %endpoint.address,
                %final_version,
                current_versions = ?current_versions,
                "Not all firmware versions match"
            );
        }

        if versions_match_final_version {
            // Done waiting, go back to overall checking of version`2s
            tracing::debug!(%machine_id, "Done waiting to reach version");
            Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                HostReprovisionState::CheckingFirmwareRepeatV2 {
                    firmware_type: Some(*firmware_type),
                    firmware_number: *firmware_number,
                },
                state.managed_state.get_host_repro_retry_count(),
            )))
        } else {
            if !self.no_firmware_update_reset_retries
                && let Some(previous_reset_time) = previous_reset_time
                && previous_reset_time + 30 * 60 <= Utc::now().timestamp()
            {
                if *reset_retry_count >= MAX_NEW_FIRMWARE_REPORTED_RESET_RETRIES {
                    let reason = format!(
                        "firmware version did not converge after completed update for {firmware_type}: expected {final_version}, found {current_versions:?} after {reset_retry_count} reset retries"
                    );
                    tracing::warn!(
                        %machine_id,
                        firmware_type = %firmware_type,
                        expected_version = %final_version,
                        current_versions = ?current_versions,
                        reset_retry_count = *reset_retry_count,
                        "Firmware version did not converge after completed update",
                    );
                    return Ok(StateHandlerOutcome::transition(scenario.actual_new_state(
                        HostReprovisionState::FailedFirmwareUpgrade {
                            firmware_type: *firmware_type,
                            report_time: Some(Utc::now()),
                            reason: Some(reason),
                        },
                        state.managed_state.get_host_repro_retry_count(),
                    )));
                }

                tracing::info!(
                    %machine_id,
                    bmc_ip_address = %endpoint.address,
                    firmware_type = ?firmware_type,
                    "Upgrade has taken more than 30 minutes to report new version; resetting again."
                );
                let details = &HostReprovisionState::ResetForNewFirmware {
                    final_version: final_version.to_string(),
                    firmware_type: *firmware_type,
                    firmware_number: *firmware_number,
                    power_drains_needed: None,
                    delay_until: None,
                    last_power_drain_operation: None,
                    reset_retry_count: *reset_retry_count + 1,
                };
                return self
                    .host_reset_for_new_firmware(state, ctx, machine_id, details, scenario)
                    .await;
            }
            tracing::info!(
                %machine_id,
                firmware_type = ?firmware_type,
                %final_version,
                current_versions = ?current_versions,
                "Waiting to reach firmware version"
            );

            let mut txn = ctx.services.db_pool.begin().await?;
            db::explored_endpoints::re_explore_if_version_matches(
                endpoint.address,
                endpoint.report_version,
                &mut txn,
            )
            .await?;
            Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
        }
    }

    fn is_auto_approved(&self) -> bool {
        let Some(ref period) = self.instance_autoreboot_period else {
            return false;
        };
        let start = period.start;
        let end = period.end;

        let now = chrono::Utc::now();

        now > start && now < end
    }
}

#[derive(Debug, Default)]
struct UpdateScriptManager {
    active: Mutex<HashMap<String, Option<bool>>>,
}

impl UpdateScriptManager {
    fn started(&self, id: String) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.insert(id, None);
    }

    fn completed(&self, id: String, success: bool) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.insert(id, Some(success));
    }

    fn clear(&self, id: &String) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.remove(id);
    }

    fn state(&self, id: &String) -> Option<bool> {
        let hashmap = self.active.lock().expect("lock poisoned");
        *hashmap.get(id).unwrap_or(&None)
    }
}

#[derive(Clone, Default, Debug)]
struct AsyncFirmwareUploader {
    active_uploads: Arc<Mutex<HashMap<String, Option<UploadResult>>>>,
}

impl AsyncFirmwareUploader {
    fn start_upload(
        &self,
        id: String,
        redfish_client: Box<dyn Redfish>,
        filename: std::path::PathBuf,
        redfish_component_type: libredfish::model::update_service::ComponentType,
        address: String,
    ) {
        if self.upload_status(&id).is_some() {
            // This situation can happen during an upgrade (typically a config upgrade) where the new instance of carbide-api starts an upgrade,
            // the old one sees that it's not the uploader and returns us to Checking, then the new one is following this path.  As we would be
            // trying to return to the exact same state that we generated before and the upload is already in progress, all we need to do here is
            // return.  It's possible that we may fluctuate the state a few times, but once the old instance dies we will be fine.
            //
            // In the odd situation where the old one was doing the upload, a similar thing will happen, but when the old one dies it will kill
            // the upload and the restart is the correct thing to do.
            //
            // Log it so we can see what's going on in case there's problems.
            tracing::info!(
                machine_id = %id,
                bmc_ip_address = %address,
                "Uploading conflict; our upload should still be in progress."
            );
            return;
        }
        // We set a None value to indicate that we know about this.  If we restart and we're in the next state but it's not set, we'll not find anything and know that the connection was reset.
        self.active_uploads
            .lock()
            .expect("lock poisoned")
            .insert(id.clone(), None);

        let active_uploads = self.active_uploads.clone();
        tokio::spawn(async move {
            match redfish_client
                .update_firmware_multipart(
                    filename.as_path(),
                    true,
                    std::time::Duration::from_secs(3600),
                    redfish_component_type,
                )
                .await
            {
                Ok(task_id) => {
                    let mut hashmap = active_uploads.lock().expect("lock poisoned");
                    hashmap.insert(id, Some(UploadResult::Success { task_id }));
                }
                Err(e) => {
                    tracing::warn!(
                        machine_id = %id,
                        bmc_ip_address = %address,
                        error = %e,
                        "Failed uploading firmware"
                    );
                    let mut hashmap = active_uploads.lock().expect("lock poisoned");
                    hashmap.insert(id, Some(UploadResult::Failure));
                }
            };
        });
    }
    fn upload_status(&self, id: &String) -> Option<Option<UploadResult>> {
        let hashmap = self.active_uploads.lock().expect("lock poisoned");
        hashmap.get(id).cloned()
    }
    fn finish_upload(&self, id: &String) {
        let mut hashmap = self.active_uploads.lock().expect("lock poisoned");
        hashmap.remove(id);
    }
}

#[track_caller]
fn handler_restart_dpu(
    machine: &Machine,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_used_for_ingestion: bool,
) -> impl Future<Output = Result<(), StateHandlerError>> {
    let trigger_location = std::panic::Location::caller();
    async move {
        tracing::info!(
            dpu_machine_id = %machine.id,
            %trigger_location,
            "attempting DPU restart"
        );

        if let Err(error) = restart_dpu(machine, ctx.services, dpf_used_for_ingestion).await {
            tracing::error!(
                dpu_machine_id = %machine.id,
                %trigger_location,
                error = %error,
                "DPU restart failed"
            );
            return Err(error);
        }

        ctx.pending_db_writes
            .push(MachineWriteOp::UpdateRebootRequestedTime {
                machine_id: machine.id,
                mode: model::machine::MachineLastRebootRequestedMode::Reboot,
                time: Utc::now(),
            });
        tracing::info!(
            dpu_machine_id = %machine.id,
            %trigger_location,
            "DPU restart triggered"
        );

        Ok(())
    }
}

pub async fn host_power_state(
    redfish_client: &dyn Redfish,
) -> Result<libredfish::PowerState, StateHandlerError> {
    redfish_client
        .get_power_state()
        .await
        .map_err(|e| redfish_error("get_power_state", e))
}

/// Returns true if the host's current Redfish power state is `Off`.
async fn is_host_powered_off(
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<bool, StateHandlerError> {
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;
    let power_state = host_power_state(redfish_client.as_ref()).await?;
    Ok(power_state == libredfish::PowerState::Off)
}

fn requires_manual_firmware_upgrade(
    state: &ManagedHostStateSnapshot,
    firmware_global: &FirmwareGlobal,
) -> bool {
    if !firmware_global.requires_manual_upgrade {
        return false;
    }

    let is_mnnvl_capable = state
        .host_snapshot
        .status
        .hardware_info
        .as_ref()
        .is_some_and(|hi| hi.is_mnnvl_capable());

    if !is_mnnvl_capable {
        return false;
    }

    state
        .host_snapshot
        .manual_firmware_upgrade_completed
        .is_none()
}

fn get_next_state_boss_job_failure(
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<(ManagedHostState, PowerState), StateHandlerError> {
    let (next_state, expected_power_state) = match &mh_snapshot.host_snapshot.state.value {
        ManagedHostState::WaitingForCleanup {
            cleanup_state,
            cleanup_context,
        } => match cleanup_state {
            CleanupState::SecureEraseBoss {
                secure_erase_boss_context,
            } => match &secure_erase_boss_context.secure_erase_boss_state {
                SecureEraseBossState::HandleJobFailure {
                    failure,
                    power_state,
                } => match power_state {
                    PowerState::Off => (
                        waiting_for_cleanup_state(
                            CleanupState::SecureEraseBoss {
                                secure_erase_boss_context: SecureEraseBossContext {
                                    boss_controller_id: secure_erase_boss_context
                                        .boss_controller_id
                                        .clone(),
                                    secure_erase_jid: None,
                                    iteration: secure_erase_boss_context.iteration,
                                    secure_erase_boss_state:
                                        SecureEraseBossState::HandleJobFailure {
                                            failure: failure.to_string(),
                                            power_state: PowerState::On,
                                        },
                                },
                            },
                            *cleanup_context,
                        ),
                        *power_state,
                    ),
                    PowerState::On => (
                        waiting_for_cleanup_state(
                            CleanupState::SecureEraseBoss {
                                secure_erase_boss_context: SecureEraseBossContext {
                                    boss_controller_id: secure_erase_boss_context
                                        .boss_controller_id
                                        .clone(),
                                    secure_erase_jid: None,
                                    iteration: Some(
                                        secure_erase_boss_context.iteration.unwrap_or_default() + 1,
                                    ),
                                    secure_erase_boss_state: SecureEraseBossState::SecureEraseBoss,
                                },
                            },
                            *cleanup_context,
                        ),
                        *power_state,
                    ),
                    _ => {
                        return Err(StateHandlerError::GenericError(eyre::eyre!(
                            "unexpected SecureEraseBossState::HandleJobFailure power_state for {}: {:#?}",
                            mh_snapshot.host_snapshot.id,
                            mh_snapshot.host_snapshot.state,
                        )));
                    }
                },
                _ => {
                    return Err(StateHandlerError::GenericError(eyre::eyre!(
                        "unexpected SecureEraseBossState state for {}: {:#?}",
                        mh_snapshot.host_snapshot.id,
                        mh_snapshot.host_snapshot.state,
                    )));
                }
            },
            CleanupState::CreateBossVolume {
                create_boss_volume_context,
            } => match &create_boss_volume_context.create_boss_volume_state {
                CreateBossVolumeState::HandleJobFailure {
                    failure,
                    power_state,
                } => match power_state {
                    PowerState::Off => (
                        waiting_for_cleanup_state(
                            CleanupState::CreateBossVolume {
                                create_boss_volume_context: CreateBossVolumeContext {
                                    boss_controller_id: create_boss_volume_context
                                        .boss_controller_id
                                        .clone(),
                                    create_boss_volume_jid: None,
                                    iteration: create_boss_volume_context.iteration,
                                    create_boss_volume_state:
                                        CreateBossVolumeState::HandleJobFailure {
                                            failure: failure.to_string(),
                                            power_state: PowerState::On,
                                        },
                                },
                            },
                            *cleanup_context,
                        ),
                        *power_state,
                    ),
                    PowerState::On => (
                        waiting_for_cleanup_state(
                            CleanupState::CreateBossVolume {
                                create_boss_volume_context: CreateBossVolumeContext {
                                    boss_controller_id: create_boss_volume_context
                                        .boss_controller_id
                                        .clone(),
                                    create_boss_volume_jid: None,
                                    iteration: Some(
                                        create_boss_volume_context.iteration.unwrap_or_default()
                                            + 1,
                                    ),
                                    create_boss_volume_state:
                                        CreateBossVolumeState::CreateBossVolume,
                                },
                            },
                            *cleanup_context,
                        ),
                        *power_state,
                    ),
                    _ => {
                        return Err(StateHandlerError::GenericError(eyre::eyre!(
                            "unexpected CreateBossVolumeState::HandleJobFailure power state for {}: {:#?}",
                            mh_snapshot.host_snapshot.id,
                            mh_snapshot.host_snapshot.state,
                        )));
                    }
                },
                _ => {
                    return Err(StateHandlerError::GenericError(eyre::eyre!(
                        "unexpected CreateBossVolume state for {}: {:#?}",
                        mh_snapshot.host_snapshot.id,
                        mh_snapshot.host_snapshot.state,
                    )));
                }
            },
            _ => {
                return Err(StateHandlerError::GenericError(eyre::eyre!(
                    "unexpected WaitingForCleanup state for {}: {:#?}",
                    mh_snapshot.host_snapshot.id,
                    mh_snapshot.host_snapshot.state,
                )));
            }
        },
        _ => {
            return Err(StateHandlerError::GenericError(eyre::eyre!(
                "unexpected host state for {}: {:#?}",
                mh_snapshot.host_snapshot.id,
                mh_snapshot.host_snapshot.state,
            )));
        }
    };
    Ok((next_state, expected_power_state))
}

fn handle_boss_controller_job_error(
    boss_controller_id: String,
    iterations: u32,
    secure_erase_boss_controller: bool,
    cleanup_context: CleanupContext,
    err: StateHandlerError,
    time_since_state_change: chrono::TimeDelta,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // Wait for 5 minutes before declaring a true failure and transition to the error handling state.
    // As we use this function to handle two different kinds of errors (and maybe others in the future),
    // the defensive nature of this check will be broadly helpful to differentiate between transient errors
    // and true failures. Here is one particular edge case:
    // It takes a little time between creating and scheduling the secure erase job.
    // If the state machine queries the BMC for the job's state prior to the job being scheduled,
    // the BMC's job service will return a 404. Wait here for five minutes to ensure
    // that the job is scheduled prior to declaring an error.
    if time_since_state_change.num_minutes() < 5 {
        return Err(err);
    }

    // we have retried this operation too many times, lets wait for manual intervention
    if iterations > 3 {
        let action = match secure_erase_boss_controller {
            true => "secure erase",
            false => "create the R1 volume on",
        };

        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "we have gone through {} iterations of trying to {action} the BOSS controller; waiting for manual intervention: {err}",
            iterations
        )));
    }

    // failure path
    let cleanup_state = match secure_erase_boss_controller {
        // the job to decomission the boss controller failed--lets retry
        true => CleanupState::SecureEraseBoss {
            secure_erase_boss_context: SecureEraseBossContext {
                boss_controller_id,
                secure_erase_jid: None,
                secure_erase_boss_state: SecureEraseBossState::HandleJobFailure {
                    failure: err.to_string(),
                    power_state: PowerState::Off,
                },
                iteration: Some(iterations),
            },
        },
        // the job to crate the R1 Volume on top of the BOSS controller failed--lets retry
        false => CleanupState::CreateBossVolume {
            create_boss_volume_context: CreateBossVolumeContext {
                boss_controller_id,
                create_boss_volume_jid: None,
                create_boss_volume_state: CreateBossVolumeState::HandleJobFailure {
                    failure: err.to_string(),
                    power_state: PowerState::Off,
                },
                iteration: Some(iterations),
            },
        },
    };

    let next_state = waiting_for_cleanup_state(cleanup_state, cleanup_context);

    Ok(StateHandlerOutcome::transition(next_state))
}

async fn wait_for_boss_controller_job_to_scheduled(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    boss_controller_id: String,
    job_id: String,
    iteration: Option<u32>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let cleanup_context = current_cleanup_context(mh_snapshot)?;
    let job_state = match redfish_client.get_job_state(&job_id).await {
        Ok(state) => state,
        Err(e) => {
            return handle_boss_controller_job_error(
                boss_controller_id,
                iteration.unwrap_or_default(),
                false,
                cleanup_context,
                redfish_error("get_job_state", e),
                mh_snapshot.host_snapshot.state.version.since_state_change(),
            );
        }
    };

    let next_state = match job_state {
        libredfish::JobState::Scheduled => waiting_for_cleanup_state(
            CleanupState::CreateBossVolume {
                create_boss_volume_context: CreateBossVolumeContext {
                    boss_controller_id,
                    create_boss_volume_jid: Some(job_id),
                    create_boss_volume_state: CreateBossVolumeState::RebootHost,
                    iteration,
                },
            },
            cleanup_context,
        ),
        libredfish::JobState::Completed => {
            tracing::warn!(
                %job_id,
                machine_id = %mh_snapshot.host_snapshot.id,
                "CreateBossVolume: job completed before being scheduled, skipping reboot",
            );

            waiting_for_cleanup_state(
                CleanupState::CreateBossVolume {
                    create_boss_volume_context: CreateBossVolumeContext {
                        boss_controller_id,
                        create_boss_volume_jid: Some(job_id),
                        create_boss_volume_state: CreateBossVolumeState::WaitForJobCompletion,
                        iteration,
                    },
                },
                cleanup_context,
            )
        }
        _ if job_state.is_error_state() => {
            return handle_boss_controller_job_error(
                boss_controller_id,
                iteration.unwrap_or_default(),
                false,
                cleanup_context,
                StateHandlerError::GenericError(eyre::eyre!(
                    "CreateBossVolume: job {} failed for {} with state {job_state:#?}",
                    job_id,
                    mh_snapshot.host_snapshot.id,
                )),
                mh_snapshot.host_snapshot.state.version.since_state_change(),
            );
        }
        _ => {
            return Ok(StateHandlerOutcome::wait(format!(
                "waiting for job {:#?} to be scheduled; current state: {job_state:#?}",
                job_id
            )));
        }
    };

    Ok(StateHandlerOutcome::transition(next_state))
}

async fn wait_for_boss_controller_job_to_complete(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let cleanup_context = current_cleanup_context(mh_snapshot)?;
    let (boss_controller_id, boss_job_id, iterations, secure_erase_boss_controller) =
        match &mh_snapshot.host_snapshot.state.value {
            ManagedHostState::WaitingForCleanup { cleanup_state, .. } => match cleanup_state {
                CleanupState::SecureEraseBoss {
                    secure_erase_boss_context,
                } => match &secure_erase_boss_context.secure_erase_boss_state {
                    SecureEraseBossState::WaitForJobCompletion => (
                        secure_erase_boss_context.boss_controller_id.clone(),
                        secure_erase_boss_context.secure_erase_jid.clone(),
                        secure_erase_boss_context.iteration.unwrap_or_default(),
                        // we are waiting for the secure erase job to complete
                        true,
                    ),
                    _ => {
                        return Err(StateHandlerError::GenericError(eyre::eyre!(
                            "unexpected SecureEraseBoss state for {}: {:#?}",
                            mh_snapshot.host_snapshot.id,
                            mh_snapshot.host_snapshot.state,
                        )));
                    }
                },
                CleanupState::CreateBossVolume {
                    create_boss_volume_context,
                } => match &create_boss_volume_context.create_boss_volume_state {
                    CreateBossVolumeState::WaitForJobCompletion => (
                        create_boss_volume_context.boss_controller_id.clone(),
                        create_boss_volume_context.create_boss_volume_jid.clone(),
                        create_boss_volume_context.iteration.unwrap_or_default(),
                        // we are waiting for the BOSS volume creation job to complete
                        false,
                    ),
                    _ => {
                        return Err(StateHandlerError::GenericError(eyre::eyre!(
                            "unexpected CreateBossVolume state for {}: {:#?}",
                            mh_snapshot.host_snapshot.id,
                            mh_snapshot.host_snapshot.state,
                        )));
                    }
                },
                _ => {
                    return Err(StateHandlerError::GenericError(eyre::eyre!(
                        "unexpected CreateBossVolume state for {}: {:#?}",
                        mh_snapshot.host_snapshot.id,
                        mh_snapshot.host_snapshot.state,
                    )));
                }
            },
            _ => {
                return Err(StateHandlerError::GenericError(eyre::eyre!(
                    "unexpected host state for {}: {:#?}",
                    mh_snapshot.host_snapshot.id,
                    mh_snapshot.host_snapshot.state,
                )));
            }
        };

    let job_id = match boss_job_id {
        Some(jid) => Ok(jid),
        None => Err(StateHandlerError::GenericError(eyre::eyre!(
            "could not find job ID in the state's context"
        ))),
    }?;

    let job_state = match redfish_client.get_job_state(&job_id).await {
        Ok(state) => state,
        Err(e) => {
            return handle_boss_controller_job_error(
                boss_controller_id,
                iterations,
                secure_erase_boss_controller,
                cleanup_context,
                redfish_error("get_job_state", e),
                mh_snapshot.host_snapshot.state.version.since_state_change(),
            );
        }
    };

    match job_state {
        // The job has completed; transition to next step in host cleanup
        libredfish::JobState::Completed => {
            // healthy path
            let next_state = match (secure_erase_boss_controller, cleanup_context) {
                // now that we have finished doing a secure erase of the BOSS controller
                // we can do a standard secure erase of the remaining drives through the /usr/sbin/nvme tool
                (true, _) => waiting_for_cleanup_state(
                    CleanupState::HostCleanup {
                        boss_controller_id: Some(boss_controller_id),
                    },
                    cleanup_context,
                ),
                // now that we have recreated the R1 volume on top of the BOSS controller, we can lock the host back down again.
                (false, CleanupContext::Deprovision) => waiting_for_cleanup_state(
                    CleanupState::CreateBossVolume {
                        create_boss_volume_context: CreateBossVolumeContext {
                            boss_controller_id,
                            create_boss_volume_jid: None,
                            create_boss_volume_state: CreateBossVolumeState::LockHost,
                            iteration: Some(iterations),
                        },
                    },
                    cleanup_context,
                ),
                (false, CleanupContext::InitialDiscovery) => post_cleanup_state(cleanup_context),
            };

            Ok(StateHandlerOutcome::transition(next_state))
        }
        // The job has failed; handle error
        _ if job_state.is_error_state() => handle_boss_controller_job_error(
            boss_controller_id,
            iterations,
            secure_erase_boss_controller,
            cleanup_context,
            StateHandlerError::GenericError(eyre::eyre!(
                "job {job_id} will not complete because it is in a failure state: {job_state:#?}",
            )),
            mh_snapshot.host_snapshot.state.version.since_state_change(),
        ),
        // The job is still running (hopefully...); wait for the job to complete
        _ => Ok(StateHandlerOutcome::wait(format!(
            "waiting for job {job_id} to complete; current state: {job_state:#?}"
        ))),
    }
}

async fn handle_boss_job_failure(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let (next_state, expected_power_state) = get_next_state_boss_job_failure(mh_snapshot)?;

    let current_power_state = redfish_client
        .get_power_state()
        .await
        .map_err(|e| redfish_error("get_power_state", e))?;

    match expected_power_state {
        PowerState::Off => {
            if current_power_state != libredfish::PowerState::Off {
                handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceOff).await?;

                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for {} to power down; current power state: {current_power_state}",
                    mh_snapshot.host_snapshot.id
                )));
            }

            redfish_client
                .bmc_reset(None)
                .await
                .map_err(|e| redfish_error("bmc_reset", e))?;

            Ok(StateHandlerOutcome::transition(next_state))
        }
        PowerState::On => {
            let basetime = mh_snapshot
                .host_snapshot
                .status
                .last_reboot_requested
                .as_ref()
                .map(|x| x.time)
                .unwrap_or(mh_snapshot.host_snapshot.state.version.timestamp());

            if wait(
                &basetime,
                ctx.services
                    .site_config
                    .machine_state_controller
                    .power_down_wait,
            ) {
                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for {} to power down; power_down_wait: {}",
                    mh_snapshot.host_snapshot.id,
                    ctx.services
                        .site_config
                        .machine_state_controller
                        .power_down_wait
                )));
            }

            if current_power_state != libredfish::PowerState::On {
                handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::On).await?;

                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for {} to power on; current power state: {current_power_state}",
                    mh_snapshot.host_snapshot.id,
                )));
            }

            Ok(StateHandlerOutcome::transition(next_state))
        }
        _ => Err(StateHandlerError::GenericError(eyre::eyre!(
            "unexpected expected_power_state while handling a boss job failure: {expected_power_state}"
        ))),
    }
}

#[track_caller]
pub fn handler_host_power_control(
    managedhost_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    action: SystemPowerControl,
) -> impl Future<Output = Result<(), StateHandlerError>> {
    let trigger_location = std::panic::Location::caller();
    handler_host_power_control_with_location(managedhost_snapshot, ctx, action, trigger_location)
}

pub async fn handler_host_power_control_with_location(
    managedhost_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    action: SystemPowerControl,
    location: &std::panic::Location<'_>,
) -> Result<(), StateHandlerError> {
    let mut action = action;
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&managedhost_snapshot.host_snapshot)
        .await?;

    let power_state = host_power_state(redfish_client.as_ref()).await?;

    let target_power_state_reached = (power_state == libredfish::PowerState::Off
        && (action == SystemPowerControl::ForceOff
            || action == SystemPowerControl::GracefulShutdown))
        || (power_state == libredfish::PowerState::On && action == SystemPowerControl::On);

    if target_power_state_reached {
        let machine_id = &managedhost_snapshot.host_snapshot.id;
        tracing::warn!(%machine_id, %power_state, %action, "Target power state is already reached. Skipping power control action");
    } else {
        if power_state == libredfish::PowerState::Off
            && (action == SystemPowerControl::ForceRestart
                || action == SystemPowerControl::GracefulRestart)
        {
            // A host can't be restarted if it is in power-off state.
            // In this call, power on the system. State machine restart the system in next iteration.
            tracing::warn!(%power_state, %action, "Power state is Off and requested action is restart. Trying to power on the host.");
            action = SystemPowerControl::On;
        }

        let machine = &managedhost_snapshot.host_snapshot;
        let is_restart = action == SystemPowerControl::ForceRestart
            || action == SystemPowerControl::GracefulRestart;

        if is_restart && needs_ipmi_restart(machine, ctx).await? {
            do_ipmi_restart(machine, ctx, action, location).await?;
        } else {
            host_power_control_with_location(
                redfish_client.as_ref(),
                machine,
                action,
                ctx,
                location,
            )
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("handler_host_power_control failed: {}", e))
            })?;
        }
    }

    // If host is forcedOff/ACPowercycled/On, it will impact DPU also. So DPU timestamp should also be updated
    // here.
    let dpu_impacting_actions = [
        SystemPowerControl::ForceOff,
        SystemPowerControl::ACPowercycle,
        SystemPowerControl::On,
    ];
    let should_update_dpu_timestamp = dpu_impacting_actions.contains(&action);

    if should_update_dpu_timestamp {
        for dpu_snapshot in &managedhost_snapshot.dpu_snapshots {
            ctx.pending_db_writes
                .push(MachineWriteOp::UpdateRebootRequestedTime {
                    machine_id: dpu_snapshot.id,
                    mode: machine_last_reboot_requested_mode(action),
                    time: Utc::now(),
                });
        }
    }

    Ok(())
}

async fn restart_dpu(
    machine: &Machine,
    services: &MachineStateHandlerServices,
    dpf_used_for_ingestion: bool,
) -> Result<(), StateHandlerError> {
    let dpu_redfish_client = services.create_redfish_client_from_machine(machine).await?;

    // We have seen the boot order be reset on DPUs in some edge cases (for example, after upgrading the BMC and CEC on BF3s)
    // This should take care of handling such cases. It is a no-op most of the time.
    // Skip for DPUs that get their BFB installed via redfish or DPF, they don't need to HTTP boot.
    let redfish_install = machine.status.bmc_info.supports_bfb_install()
        && services.site_config.dpu_enable_secure_boot;

    if !redfish_install && !dpf_used_for_ingestion {
        let _ = dpu_redfish_client
            .boot_once(libredfish::Boot::UefiHttp)
            .await
            .map_err(|e| {
                // We use a Dell to mock our BMC responses in the integration tests. UefiHttp boot is not implemented
                // for Dells, so this call is failing in our tests. Regardless, it is ok to not make this call blocking.
                tracing::error!(
                    machine_id = %machine.id,
                    error = %e,
                    "Failed to configure DPU to boot once"
                );
            });
    }

    let power_state = host_power_state(dpu_redfish_client.as_ref()).await?;
    let power_action = dpu_restart_power_action(power_state)?;
    if power_action == SystemPowerControl::On {
        tracing::warn!(
            machine_id = %machine.id,
            %power_state,
            "DPU is not running; powering it on instead of restarting it"
        );
    }

    dpu_redfish_client
        .power(power_action)
        .await
        .map_err(|error| redfish_error("reboot dpu", error))?;

    Ok(())
}

fn dpu_restart_power_action(
    power_state: libredfish::PowerState,
) -> Result<SystemPowerControl, StateHandlerError> {
    match power_state {
        // BlueField-3 reports its stable StandbyOffline state as Paused after
        // the Arm OS shuts down, so it needs the same recovery as Off.
        libredfish::PowerState::Off | libredfish::PowerState::Paused => Ok(SystemPowerControl::On),
        libredfish::PowerState::On => Ok(SystemPowerControl::ForceRestart),
        libredfish::PowerState::PoweringOff
        | libredfish::PowerState::PoweringOn
        | libredfish::PowerState::Reset
        | libredfish::PowerState::Unknown => Err(StateHandlerError::GenericError(eyre!(
            "cannot restart DPU while its power state is {power_state}; retrying"
        ))),
    }
}

/// Returns true if this machine needs IPMI restart to avoid killing its DPUs.
/// Redfish restart kills the DPU on some machines
async fn needs_ipmi_restart(
    machine: &Machine,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<bool, StateHandlerError> {
    let addr = machine
        .status
        .bmc_info
        .ip_addr()
        .map_err(StateHandlerError::GenericError)?;
    let endpoints =
        db::explored_endpoints::find_by_ips(&mut ctx.services.db_reader, vec![addr]).await?;
    let Some(ep) = endpoints.first() else {
        return Ok(false);
    };

    Ok(match ep.report.vendor {
        // Lenovo SR650 V4s kill power to DPUs on Redfish ForceRestart/GracefulRestart,
        // causing PXE boot failures. IPMI chassis reset avoids this.
        // https://github.com/NVIDIA/bare-metal-manager-core/issues/347
        Some(bmc_vendor::BMCVendor::Lenovo) => {
            let model = ep.report.model.as_deref().unwrap_or("");
            model.contains("SR650 V4")
        }
        Some(bmc_vendor::BMCVendor::Nvidia) => {
            ep.report.systems.iter().any(|s| s.id == "DGX")
                && ep.report.managers.iter().any(|m| m.id == "BMC")
        }
        _ => false,
    })
}

/// Perform an IPMI chassis power reset for the given machine
async fn do_ipmi_restart(
    machine: &Machine,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    action: SystemPowerControl,
    trigger_location: &std::panic::Location<'_>,
) -> Result<(), StateHandlerError> {
    tracing::info!(
        machine_id = machine.id.to_string(),
        action = action.to_string(),
        trigger_location = %trigger_location,
        "IPMI Host Power Control"
    );
    ctx.pending_db_writes
        .push(MachineWriteOp::UpdateRebootRequestedTime {
            machine_id: machine.id,
            mode: machine_last_reboot_requested_mode(action),
            time: Utc::now(),
        });

    let bmc_mac = machine
        .status
        .bmc_info
        .mac
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: machine.id.to_string(),
            missing: "bmc_mac",
        })?;
    let ip: IpAddr = machine
        .status
        .bmc_info
        .ip
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: machine.id.to_string(),
            missing: "bmc_ip",
        })?;
    let credential_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: bmc_mac,
        },
    };
    let bmc_address = resolve_ipmi_address(ip, ctx).await?;
    ctx.services
        .ipmi_tool
        .restart(&machine.id, bmc_address, false, &credential_key)
        .await
        .map_err(|e| {
            StateHandlerError::GenericError(eyre!("IPMI restart failed for {}: {}", machine.id, e))
        })
}

async fn resolve_ipmi_address(
    ip_address: IpAddr,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<SocketAddr, StateHandlerError> {
    let metadata =
        db::explored_endpoints::lookup_bmc_metadata_by_ip(ip_address, &mut ctx.services.db_reader)
            .await?;
    Ok(ipmi_socket_address(ip_address, metadata.ipmi_port))
}

fn ipmi_socket_address(ip_address: IpAddr, port: Option<u16>) -> SocketAddr {
    SocketAddr::new(ip_address, port.unwrap_or(carbide_ipmi::DEFAULT_IPMI_PORT))
}

/// find_explored_refreshed_endpoint will locate the explored endpoint for the given state.
/// It will return an error for not finding any endpoint, and Ok(None) when we're still waiting
/// on explorer to have a chance to run again.
pub async fn find_explored_refreshed_endpoint(
    state: &ManagedHostStateSnapshot,
    machine_id: &MachineId,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Option<ExploredEndpoint>, StateHandlerError> {
    let addr: IpAddr = state
        .host_snapshot
        .status
        .bmc_info
        .ip_addr()
        .map_err(StateHandlerError::GenericError)?;

    let endpoint =
        db::explored_endpoints::find_by_ips(&mut ctx.services.db_reader, vec![addr]).await?;
    let endpoint = endpoint
        .into_iter()
        .next()
        .ok_or(StateHandlerError::GenericError(
            eyre! {"unable to find explored_endpoint for {machine_id}"},
        ))?;

    if endpoint.waiting_for_explorer_refresh {
        // In the cases where this was called, we care about prompt updates, so poke site explorer to revisit this endpoint next time it runs
        ctx.pending_db_writes
            .push(MachineWriteOp::ReExploreIfVersionMatches {
                address: endpoint.address,
                version: endpoint.report_version,
            });
        return Ok(None);
    }
    Ok(Some(endpoint))
}

// If already reprovisioning is started, we can restart.
// Also check that this is not some old request. The restart requested time must be greater than
// last state change.
fn can_restart_reprovision(dpu_snapshots: &[Machine], version: ConfigVersion) -> bool {
    let mut reprov_started = false;
    let mut requested_at = vec![];
    for dpu_snapshot in dpu_snapshots {
        if let Some(reprov_req) = &dpu_snapshot.reprovision_requested {
            if reprov_req.started_at.is_some() {
                reprov_started = true;
            }
            requested_at.push(reprov_req.restart_reprovision_requested_at);
        }
    }

    if !reprov_started {
        return false;
    }

    // Get the latest time of restart requested.
    requested_at.sort();

    let Some(latest_requested_at) = requested_at.last() else {
        return false;
    };

    dpu_reprovision_restart_requested_after_state_transition(version, *latest_requested_at)
}

/// Call [`Redfish::machine_setup`], but ignore any [`RedfishError::NoDpu`] if we expect there to
/// be no DPUs on the host. Returns `Ok(Some(job_id))` when the vendor (e.g. Dell) creates a BIOS
/// config job that must complete before configuring boot order; `Ok(None)` when no job to wait for.
pub async fn call_machine_setup_and_handle_no_dpu_error(
    redfish_client: &dyn Redfish,
    boot_interface: Option<&BootInterfaceTarget>,
    expected_dpu_count: usize,
    site_config: &MachineStateHandlerSiteConfig,
) -> Result<Option<String>, RedfishError> {
    let setup_result = match boot_interface {
        Some(target) => {
            target
                .run(|bi| {
                    redfish_client.machine_setup(
                        Some(bi),
                        &site_config.bios_profiles,
                        site_config.selected_profile,
                        &site_config.oem_manager_profiles,
                    )
                })
                .await
        }
        None => {
            redfish_client
                .machine_setup(
                    None,
                    &site_config.bios_profiles,
                    site_config.selected_profile,
                    &site_config.oem_manager_profiles,
                )
                .await
        }
    };
    handle_no_dpu_error(setup_result, expected_dpu_count, "machine_setup")
}

async fn set_boot_order_dpu_first_and_handle_no_dpu_error(
    redfish_client: &dyn Redfish,
    boot_interface: &BootInterfaceTarget,
    expected_dpu_count: usize,
) -> Result<Option<String>, RedfishError> {
    let setup_result = boot_interface
        .run(|bi| redfish_client.set_boot_order_dpu_first(bi))
        .await;
    handle_no_dpu_error(setup_result, expected_dpu_count, "set_boot_order_dpu_first")
}

/// Treat `Err(RedfishError::NoDpu)` as `Ok(None)` *only* when the host is
/// declared zero-DPU (`expected_dpu_count == 0`). Other error variants and
/// successful results pass through untouched. The host DPU policy gate in
/// site-explorer is what guarantees `expected_dpu_count == 0` actually
/// means the host carries no managed DPU -- either `Ignore` or `Nic`.
/// Neither has a DPU to answer Redfish, so a `NoDpu` error is expected, not a
/// fault.
fn handle_no_dpu_error(
    result: Result<Option<String>, RedfishError>,
    expected_dpu_count: usize,
    operation: &'static str,
) -> Result<Option<String>, RedfishError> {
    match (result, expected_dpu_count) {
        (Err(RedfishError::NoDpu), 0) => {
            tracing::info!(
                operation,
                "redfish operation failed with NoDpu on a zero-DPU host; treating as Ok"
            );
            Ok(None)
        }
        (other, _) => other,
    }
}

// Returns true if update_manager flagged this managed host as needing its firmware examined
async fn is_machine_validation_requested(state: &ManagedHostStateSnapshot) -> bool {
    let Some(on_demand_machine_validation_request) =
        state.host_snapshot.on_demand_machine_validation_request
    else {
        return false;
    };

    if on_demand_machine_validation_request {
        tracing::info!(machine_id = %state.host_snapshot.id, "Machine Validation is requested");
    }

    on_demand_machine_validation_request
}

async fn log_host_config(redfish_client: &dyn Redfish, mh_snapshot: &ManagedHostStateSnapshot) {
    let host_id = mh_snapshot.host_snapshot.id;
    let managed_state = &mh_snapshot.managed_state;

    let boot_options = match redfish_client.get_boot_options().await {
        Ok(opts) => opts,
        Err(e) => {
            tracing::warn!(
                host_machine_id = %host_id,
                %managed_state,
                error = %e,
                "Failed to fetch boot options"
            );
            return;
        }
    };

    let mut boot_entries = Vec::with_capacity(boot_options.members.len());
    for (i, member) in boot_options.members.iter().enumerate() {
        let option_id = member.odata_id.split('/').next_back().unwrap_or("unknown");
        match redfish_client.get_boot_option(option_id).await {
            Ok(opt) => {
                let enabled = match opt.boot_option_enabled {
                    Some(true) => "enabled",
                    Some(false) => "disabled",
                    None => "unknown",
                };
                let device_path = opt.uefi_device_path.as_deref().unwrap_or("N/A");
                boot_entries.push(format!(
                    "  #{}: {} (id={}, {}, path={})",
                    i + 1,
                    opt.display_name,
                    opt.id,
                    enabled,
                    device_path
                ));
            }
            Err(e) => {
                boot_entries.push(format!(
                    "  #{}: {} (failed to fetch details: {})",
                    i + 1,
                    option_id,
                    e
                ));
            }
        }
    }

    let pcie_section = match redfish_client.pcie_devices().await {
        Ok(devices) => {
            let entries: Vec<String> = devices
                .iter()
                .enumerate()
                .map(|(i, dev)| {
                    format!(
                        "  #{}: {} (id={}, manufacturer={}, part={}, serial={}, fw={})",
                        i + 1,
                        dev.name.as_deref().unwrap_or("N/A"),
                        dev.id.as_deref().unwrap_or("N/A"),
                        dev.manufacturer.as_deref().unwrap_or("N/A"),
                        dev.part_number.as_deref().unwrap_or("N/A"),
                        dev.serial_number.as_deref().unwrap_or("N/A"),
                        dev.firmware_version.as_deref().unwrap_or("N/A"),
                    )
                })
                .collect();
            format!("PCIe devices:\n{}", entries.join("\n"))
        }
        Err(e) => format!("PCIe devices: failed to fetch ({})", e),
    };

    tracing::info!(
        %host_id,
        %managed_state,
        "Host config:\nBoot order:\n{}\n{}",
        boot_entries.join("\n"),
        pcie_section
    );
}

/// Handles a shared boot-configuration stage during assigned-host platform
/// configuration.
///
/// Called by `handle_instance_host_platform_config` for its BIOS configuration,
/// vendor-job, polling, and boot-order states. This adapter maps continued
/// stages back into `HostPlatformConfigurationState`, proceeds to `LockHost` on
/// completion, and scopes terminal failure to the assigned instance.
async fn handle_instance_host_boot_config_stage(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    stage: HostBootConfigStage,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match run_host_boot_config_stage(
        ctx,
        reachability_params,
        redfish_client,
        mh_snapshot,
        None,
        stage,
    )
    .await?
    {
        HostBootConfigOutcome::Continue(stage) => {
            let platform_config_state = match stage {
                HostBootConfigStage::ConfigureBios { retry_count } => {
                    HostPlatformConfigurationState::ConfigureBios {
                        bios_config_info: None,
                        retry_count,
                    }
                }
                HostBootConfigStage::WaitingForBiosJob { bios_config_info } => {
                    HostPlatformConfigurationState::WaitingForBiosJob { bios_config_info }
                }
                HostBootConfigStage::PollingBiosSetup { retry_count } => {
                    HostPlatformConfigurationState::PollingBiosSetup { retry_count }
                }
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info,
                } => HostPlatformConfigurationState::SetBootOrder {
                    set_boot_order_info,
                },
            };

            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Assigned {
                    instance_state: InstanceState::HostPlatformConfiguration {
                        platform_config_state,
                    },
                },
            ))
        }
        HostBootConfigOutcome::Complete => Ok(StateHandlerOutcome::transition(
            ManagedHostState::Assigned {
                instance_state: InstanceState::HostPlatformConfiguration {
                    platform_config_state: HostPlatformConfigurationState::LockHost,
                },
            },
        )),
        HostBootConfigOutcome::Wait(reason) => Ok(StateHandlerOutcome::wait(reason)),
        HostBootConfigOutcome::Failed { failure } => Ok(StateHandlerOutcome::transition(
            ManagedHostState::Assigned {
                instance_state: InstanceState::Failed {
                    details: FailureDetails {
                        cause: FailureCause::BiosSetupFailed { err: failure },
                        failed_at: Utc::now(),
                        source: FailureSource::StateMachineArea(StateMachineArea::AssignedInstance),
                    },
                    machine_id: mh_snapshot.host_snapshot.id,
                },
            },
        )),
    }
}

async fn handle_instance_host_platform_config(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &mut ManagedHostStateSnapshot,
    reachability_params: &ReachabilityParams,
    platform_config_state: HostPlatformConfigurationState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // The deletion-only BMC factory-reset sub-flow must be dispatched before the
    // shared stored-credential client is built: during/after the reset the BMC
    // is down or on factory credentials, so the unconditional authenticated
    // client build below would fail before we reached the match. That handler
    // builds exactly the client each of its sub-states needs.
    if let HostPlatformConfigurationState::FactoryResetBmc { reset_state } = &platform_config_state
    {
        return factory_reset::handle_factory_reset_bmc(ctx, mh_snapshot, reset_state.clone())
            .await;
    }

    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;

    let instance_state = match platform_config_state {
        HostPlatformConfigurationState::PowerCycle {
            power_on,
            power_on_retry_count,
        } => {
            let power_state = get_power_state(redfish_client.as_ref()).await?;

            // Phase 1: Power OFF (power_on=false means we need to power off first)
            if !power_on {
                if power_state == PowerState::Off {
                    // Host is already off, proceed to power on phase
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::Assigned {
                            instance_state: InstanceState::HostPlatformConfiguration {
                                platform_config_state: HostPlatformConfigurationState::PowerCycle {
                                    power_on: true,
                                    power_on_retry_count: 0,
                                },
                            },
                        },
                    ));
                }

                // Host is still on, issue power off command
                host_power_control(
                    redfish_client.as_ref(),
                    &mh_snapshot.host_snapshot,
                    SystemPowerControl::ForceOff,
                    ctx,
                )
                .await
                .map_err(|e| {
                    StateHandlerError::GenericError(eyre!("failed to power off host: {}", e))
                })?;

                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for {} to power OFF; current power state: {}",
                    mh_snapshot.host_snapshot.id, power_state
                )));
            }

            // Phase 2: Power ON (power_on=true means host was off, now power it on)

            // Wait for the power-down grace period before powering back on
            let basetime = mh_snapshot
                .host_snapshot
                .status
                .last_reboot_requested
                .as_ref()
                .map(|x| x.time)
                .unwrap_or(mh_snapshot.host_snapshot.state.version.timestamp());

            if wait(&basetime, reachability_params.power_down_wait) {
                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for power-down grace period before powering on {}; power_down_wait: {}",
                    mh_snapshot.host_snapshot.id, reachability_params.power_down_wait
                )));
            }

            if power_state == PowerState::On {
                // Host is on, unlock BMC before checking config so Redfish reflects reality
                return Ok(StateHandlerOutcome::transition(
                    ManagedHostState::Assigned {
                        instance_state: InstanceState::HostPlatformConfiguration {
                            platform_config_state: HostPlatformConfigurationState::UnlockHost {
                                unlock_host_state: UnlockHostState::DisableLockdown,
                            },
                        },
                    },
                ));
            }

            // Host is still off. Every 5th retry use AC power cycle instead of On.
            let next_retry = power_on_retry_count + 1;
            if next_retry % 5 == 0 {
                match host_power_control(
                    redfish_client.as_ref(),
                    &mh_snapshot.host_snapshot,
                    SystemPowerControl::ACPowercycle,
                    ctx,
                )
                .await
                {
                    Ok(()) => {
                        return Ok(StateHandlerOutcome::transition(
                            ManagedHostState::Assigned {
                                instance_state: InstanceState::HostPlatformConfiguration {
                                    platform_config_state:
                                        HostPlatformConfigurationState::PowerCycle {
                                            power_on: true,
                                            power_on_retry_count: next_retry,
                                        },
                                },
                            },
                        ));
                    }
                    Err(RedfishError::NotSupported(_)) => {
                        // if not supported, just power on
                        tracing::info!("AC Powercycle not supported, skipping to power on");
                    }
                    Err(e) => {
                        // TODO: Dell's return a generic error if in lockdown which needs to be changed in Redfish SDK
                        tracing::warn!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            error = %e,
                            "Failed to AC Powercycle host, skipping to power on"
                        );
                    }
                };
            }

            host_power_control(
                redfish_client.as_ref(),
                &mh_snapshot.host_snapshot,
                SystemPowerControl::On,
                ctx,
            )
            .await
            .map_err(|e| StateHandlerError::GenericError(eyre!("failed to power on host: {e}")))?;

            tracing::info!(
                host_machine_id = %mh_snapshot.host_snapshot.id,
                power_on_retry_count = next_retry,
                %power_state,
                "waiting for host to power ON"
            );
            return Ok(StateHandlerOutcome::transition(
                ManagedHostState::Assigned {
                    instance_state: InstanceState::HostPlatformConfiguration {
                        platform_config_state: HostPlatformConfigurationState::PowerCycle {
                            power_on: true,
                            power_on_retry_count: next_retry,
                        },
                    },
                },
            ));
        }
        HostPlatformConfigurationState::UnlockHost { unlock_host_state } => {
            match unlock_host_state {
                UnlockHostState::DisableLockdown => {
                    redfish_client
                        .lockdown_bmc(EnabledDisabled::Disabled)
                        .await
                        .map_err(|e| redfish_error("lockdown_bmc", e))?;

                    let vendor = mh_snapshot.host_snapshot.bmc_vendor();

                    // Supermicro BMCs in lockdown mode sometimes report stale boot order
                    // via Redfish (https://github.com/NVIDIA/bare-metal-manager-core/issues/505).
                    // A reboot with lockdown disabled forces the BMC to re-read the actual UEFI
                    // boot configuration.
                    if vendor.is_supermicro() {
                        tracing::info!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            %vendor,
                            "BMC lockdown disabled; rebooting host so Redfish reflects actual boot order"
                        );
                        InstanceState::HostPlatformConfiguration {
                            platform_config_state: HostPlatformConfigurationState::UnlockHost {
                                unlock_host_state: UnlockHostState::RebootHost,
                            },
                        }
                    } else {
                        tracing::info!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            %vendor,
                            "BMC lockdown disabled; skipping post-unlock reboot (not required for this vendor)"
                        );
                        InstanceState::HostPlatformConfiguration {
                            platform_config_state: HostPlatformConfigurationState::CheckHostConfig,
                        }
                    }
                }
                UnlockHostState::RebootHost => {
                    host_power_control(
                        redfish_client.as_ref(),
                        &mh_snapshot.host_snapshot,
                        SystemPowerControl::ForceRestart,
                        ctx,
                    )
                    .await
                    .map_err(|e| {
                        StateHandlerError::GenericError(eyre!(
                            "failed to ForceRestart host after disabling BMC lockdown: {}",
                            e
                        ))
                    })?;

                    InstanceState::HostPlatformConfiguration {
                        platform_config_state: HostPlatformConfigurationState::UnlockHost {
                            unlock_host_state: UnlockHostState::WaitForUefiBoot,
                        },
                    }
                }
                UnlockHostState::WaitForUefiBoot => {
                    let entered_at = mh_snapshot.host_snapshot.state.version.timestamp();
                    if wait(&entered_at, reachability_params.uefi_boot_wait) {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for UEFI boot to complete on {} after post-unlock reboot; \
                             wait duration: {}, will proceed after {}",
                            mh_snapshot.host_snapshot.id,
                            reachability_params.uefi_boot_wait,
                            entered_at + reachability_params.uefi_boot_wait,
                        )));
                    }

                    InstanceState::HostPlatformConfiguration {
                        platform_config_state: HostPlatformConfigurationState::CheckHostConfig,
                    }
                }
            }
        }
        HostPlatformConfigurationState::CheckHostConfig => {
            match check_host_boot_config(
                redfish_client.as_ref(),
                mh_snapshot,
                reachability_params,
                HostBootConfigDpuFreshness::CurrentHostState,
                None,
                ctx,
            )
            .await?
            {
                HostBootConfigCheckOutcome::Wait(reason) => {
                    return Ok(StateHandlerOutcome::wait(reason));
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::ConfigureBios) => {
                    InstanceState::HostPlatformConfiguration {
                        platform_config_state: HostPlatformConfigurationState::ConfigureBios {
                            bios_config_info: None,
                            retry_count: 0,
                        },
                    }
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::SetBootOrder) => {
                    InstanceState::HostPlatformConfiguration {
                        platform_config_state: HostPlatformConfigurationState::SetBootOrder {
                            set_boot_order_info: initial_set_boot_order_info(),
                        },
                    }
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::Complete) => {
                    InstanceState::HostPlatformConfiguration {
                        platform_config_state: HostPlatformConfigurationState::LockHost,
                    }
                }
            }
        }
        HostPlatformConfigurationState::ConfigureBios {
            bios_config_info,
            retry_count,
        } => {
            // Legacy persisted state: migrate to WaitingForBiosJob (one transition per invocation).
            if let Some(info) = bios_config_info {
                return Ok(StateHandlerOutcome::transition(
                    ManagedHostState::Assigned {
                        instance_state: InstanceState::HostPlatformConfiguration {
                            platform_config_state:
                                HostPlatformConfigurationState::WaitingForBiosJob {
                                    bios_config_info: info,
                                },
                        },
                    },
                ));
            }

            return handle_instance_host_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                HostBootConfigStage::ConfigureBios { retry_count },
            )
            .await;
        }
        HostPlatformConfigurationState::WaitingForBiosJob { bios_config_info } => {
            return handle_instance_host_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                HostBootConfigStage::WaitingForBiosJob { bios_config_info },
            )
            .await;
        }
        HostPlatformConfigurationState::PollingBiosSetup { retry_count } => {
            return handle_instance_host_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                HostBootConfigStage::PollingBiosSetup { retry_count },
            )
            .await;
        }
        HostPlatformConfigurationState::SetBootOrder {
            set_boot_order_info,
        } => {
            return handle_instance_host_boot_config_stage(
                ctx,
                mh_snapshot,
                reachability_params,
                redfish_client.as_ref(),
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info,
                },
            )
            .await;
        }
        HostPlatformConfigurationState::LockHost => {
            if mh_snapshot.host_snapshot.host_profile.disable_lockdown {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "Skipping lockdown re-enable in platform config per expected-machine config"
                );
            } else {
                redfish_client
                    .lockdown_bmc(EnabledDisabled::Enabled)
                    .await
                    .map_err(|e| redfish_error("lockdown_bmc", e))?;
            }

            InstanceState::WaitingForDpusToUp
        }
        HostPlatformConfigurationState::FactoryResetBmc { .. } => {
            // We should never get here. `FactoryResetBmc` is dispatched at the top
            // of this function, before the shared authenticated client is built, and returns there.
            // Reaching this arm means that early dispatch was removed or bypassed
            // -- a logic bug.
            tracing::error!(
                machine_id = %mh_snapshot.host_snapshot.id,
                "FactoryResetBmc reached the shared host-platform-config match; early dispatch to handle_factory_reset_bmc was bypassed"
            );
            return Err(StateHandlerError::InvalidState(format!(
                "FactoryResetBmc must be dispatched by handle_factory_reset_bmc before the shared client build (machine {})",
                mh_snapshot.host_snapshot.id
            )));
        }
    };

    let next_state = ManagedHostState::Assigned { instance_state };

    Ok(StateHandlerOutcome::transition(next_state))
}

async fn set_host_boot_order(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    explicit_target: Option<&BootInterfaceTarget>,
    set_boot_order_info: SetBootOrderInfo,
) -> Result<SetBootOrderOutcome, StateHandlerError> {
    let boot_interface_resolution =
        resolve_boot_interface_for_step(ctx, mh_snapshot, explicit_target).await?;
    match set_boot_order_info.set_boot_order_state {
        SetBootOrderState::SetBootOrder => {
            // There used to be a `force_dpu_nic_mode`-gated short-circuit
            // here that, for zero-DPU hosts when the flag was set, called
            // `boot_first(Boot::UefiHttp)` and returned `Done` to skip
            // the rest of the SetBootOrder flow. Dropped along with the
            // flag. We don't extend the `boot_first(UefiHttp)` call to
            // all zero-DPU hosts because libredfish doesn't implement
            // `boot_first` for every vendor yet (Dell currently returns
            // `NotSupported`); zero-DPU hosts fall through to the
            // `set_boot_order_dpu_first` path below, which treats the
            // resulting `NoDpu` error as `Ok` and still hits `CheckBootOrder`
            // for verification.
            //
            // Resolve the boot NIC the same way `CheckHostConfig` does,
            // supporting hosts with DPU(s) and zero DPUs alike. Before the first
            // HostInband lease creates a real row, a zero-DPU/NIC-mode host
            // resolves via its predictions; it waits only when neither a real row
            // nor a usable prediction exists.
            let boot_interface = match require_boot_interface(
                &mh_snapshot.host_snapshot.id,
                boot_interface_resolution,
                "setting boot order",
                SetBootOrderOutcome::Wait,
            )? {
                RequiredBootInterface::Ready(target) => target,
                RequiredBootInterface::Wait(outcome) => return Ok(outcome),
            };

            // Don't re-apply a boot config that's already in place. On Dell two
            // BIOS config writes cannot share one pending job: machine_setup can
            // commit a job that collides with set_boot_order_dpu_first (iDRAC
            // `SYS011`). If the HTTP device drifted since the owning lifecycle's
            // inspection, return to the shared BIOS/job stages before attempting
            // the order write.
            let is_bios_setup = boot_interface
                .run(|bi| redfish_client.is_bios_setup(Some(bi)))
                .await
                .map_err(|e| redfish_error("is_bios_setup", e))?;

            if !is_bios_setup {
                // The HTTP-boot device drifted after the owning lifecycle's
                // inspection. Route back through the shared BIOS driver rather
                // than issuing machine_setup here: vendor jobs must be persisted
                // and polled before boot-order work can safely continue.
                return Ok(SetBootOrderOutcome::ConfigureBios);
            }

            // Keep the vendor safety policy at the actuator boundary. This is
            // intentionally after the BIOS check: validation reuses this state
            // to restore a reverted HTTP boot device on Viking, where BIOS
            // repair is supported but boot-order remediation is not.
            if should_skip_boot_order_remediation(mh_snapshot) {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    bmc_vendor = %mh_snapshot.host_snapshot.bmc_vendor(),
                    "Skipping boot order remediation on Viking (known FW/BMC issue)"
                );
                return Ok(SetBootOrderOutcome::Done);
            }

            // HTTP-boot device is already set, so `machine_setup` was not re-run
            // this pass -- no pending BIOS job for the boot-order set to collide
            // with. If the boot order is already set too, there's nothing to
            // re-apply: go straight to verification.
            let is_boot_order_setup = boot_interface
                .run(|bi| redfish_client.is_boot_order_setup(bi))
                .await
                .map_err(|e| redfish_error("is_boot_order_setup", e))?;

            if is_boot_order_setup {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "Boot config already in place at SetBootOrder; skipping re-apply and verifying",
                );
                return Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                    set_boot_order_jid: None,
                    set_boot_order_state: SetBootOrderState::CheckBootOrder,
                    retry_count: set_boot_order_info.retry_count,
                }));
            }

            let jid = match set_boot_order_dpu_first_and_handle_no_dpu_error(
                redfish_client,
                &boot_interface,
                mh_snapshot.host_snapshot.associated_dpu_machine_ids().len(),
            )
            .await
            {
                Ok(jid) => jid,
                Err(e) => {
                    // TODO(chet): I don't know if this is still a thing, but I'm pretty sure
                    // it hasn't been fixed/addressed yet, and it appears to be logged all over
                    // the place now.
                    tracing::warn!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        error = %e,
                        "redfish set_boot_order_dpu_first failed, potentially due to known race condition between UEFI POST and BMC. triggering force-restart if needed"
                    );

                    let reboot_status = if mh_snapshot
                        .host_snapshot
                        .status
                        .last_reboot_requested
                        .is_none()
                    {
                        handler_host_power_control(
                            mh_snapshot,
                            ctx,
                            SystemPowerControl::ForceRestart,
                        )
                        .await?;

                        RebootStatus {
                            increase_retry_count: true,
                            status: "Restarted host".to_string(),
                        }
                    } else {
                        trigger_reboot_if_needed(
                            &mh_snapshot.host_snapshot,
                            mh_snapshot,
                            None,
                            reachability_params,
                            ctx,
                        )
                        .await?
                    };

                    // Log boot options and PCIe device list whenever a fresh reboot is
                    // triggered so we capture full diagnostic context (UEFI device paths +
                    // PCIe inventory) before state resets. Skipped when waiting on an
                    // already-in-progress reboot to avoid redundant Redfish calls.
                    if reboot_status.increase_retry_count {
                        log_host_config(redfish_client, mh_snapshot).await;
                    }

                    // Return WaitingForReboot instead of Err to ensure the transaction is
                    // committed, and last_reboot_requested is persisted. Returning Err would
                    // cause a transaction rollback, leading to a tight reboot loop (since the
                    // reboot timestamp would be lost).
                    return Ok(SetBootOrderOutcome::WaitingForReboot(format!(
                        "redfish set_boot_order_dpu_first failed: {e}; triggered host reboot: {reboot_status:#?}"
                    )));
                }
            };

            Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                set_boot_order_jid: jid,
                set_boot_order_state: SetBootOrderState::WaitForSetBootOrderJobScheduled,
                retry_count: set_boot_order_info.retry_count,
            }))
        }
        SetBootOrderState::WaitForHttpBootDeviceApplied => {
            // Backward compatibility for hosts persisted in the former inline
            // machine_setup flow. Once its wait window expires, migrate into the
            // shared BIOS/job stages so any new vendor job is retained.
            const HTTP_BOOT_DEVICE_APPLY_WAIT_MINUTES: i64 = 10;
            const MAX_HTTP_BOOT_DEVICE_APPLY_RETRIES: u32 = 3;

            let boot_interface = match require_boot_interface(
                &mh_snapshot.host_snapshot.id,
                boot_interface_resolution,
                "verifying the re-asserted HTTP boot device",
                SetBootOrderOutcome::Wait,
            )? {
                RequiredBootInterface::Ready(target) => target,
                RequiredBootInterface::Wait(outcome) => return Ok(outcome),
            };

            let is_bios_setup = boot_interface
                .run(|bi| redfish_client.is_bios_setup(Some(bi)))
                .await
                .map_err(|e| redfish_error("is_bios_setup", e))?;

            if is_bios_setup {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "Re-asserted HTTP boot device applied; returning to SetBootOrder to set the boot order",
                );
                return Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                    set_boot_order_jid: None,
                    set_boot_order_state: SetBootOrderState::SetBootOrder,
                    retry_count: set_boot_order_info.retry_count,
                }));
            }

            let minutes_waiting = mh_snapshot
                .host_snapshot
                .state
                .version
                .since_state_change()
                .num_minutes();

            if minutes_waiting >= HTTP_BOOT_DEVICE_APPLY_WAIT_MINUTES {
                // Preserve the terminal behavior of a legacy state whose old
                // retry budget was already exhausted. Any nonterminal state
                // carries its existing count into the shared convergence
                // budget.
                if set_boot_order_info.retry_count >= MAX_HTTP_BOOT_DEVICE_APPLY_RETRIES {
                    return Err(StateHandlerError::ManualInterventionRequired(format!(
                        "HTTP boot device on host {} still not applied after {} re-asserts; manual intervention required",
                        mh_snapshot.host_snapshot.id, MAX_HTTP_BOOT_DEVICE_APPLY_RETRIES
                    )));
                }
                tracing::warn!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    time_in_state_minutes = minutes_waiting,
                    "Re-asserted HTTP boot device still not applied after the wait window; migrating to shared BIOS repair",
                );
                return Ok(SetBootOrderOutcome::ConfigureBios);
            }

            Ok(SetBootOrderOutcome::WaitingForReboot(format!(
                "Waiting for the re-asserted HTTP boot device to apply on host {} ({minutes_waiting}m of {HTTP_BOOT_DEVICE_APPLY_WAIT_MINUTES}m)",
                mh_snapshot.host_snapshot.id
            )))
        }
        SetBootOrderState::WaitForSetBootOrderJobScheduled => {
            if let Some(job_id) = &set_boot_order_info.set_boot_order_jid {
                let job_state = redfish_client
                    .get_job_state(job_id)
                    .await
                    .map_err(|e| redfish_error("get_job_state", e))?;

                if job_state.is_error_state() {
                    tracing::warn!(
                        %job_id,
                        machine_id = %mh_snapshot.host_snapshot.id,
                        job_state = ?job_state,
                        "SetBootOrder: job failed while waiting for scheduling, transitioning to HandleJobFailure",
                    );

                    return Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                        set_boot_order_jid: None,
                        set_boot_order_state: SetBootOrderState::HandleJobFailure {
                            failure: format!(
                                "Job {job_id} failed while waiting to be scheduled: {job_state:#?}"
                            ),
                            power_state: PowerState::Off,
                        },
                        retry_count: set_boot_order_info.retry_count,
                    }));
                }

                if matches!(job_state, libredfish::JobState::Completed) {
                    tracing::info!(
                        %job_id,
                        machine_id = %mh_snapshot.host_snapshot.id,
                        "SetBootOrder: job completed before scheduling was observed; skipping reboot",
                    );

                    return Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                        set_boot_order_jid: set_boot_order_info.set_boot_order_jid.clone(),
                        set_boot_order_state: SetBootOrderState::CheckBootOrder,
                        retry_count: set_boot_order_info.retry_count,
                    }));
                }

                if !matches!(job_state, libredfish::JobState::Scheduled) {
                    return Err(StateHandlerError::GenericError(eyre::eyre!(
                        "waiting for job {:#?} to be scheduled; current state: {job_state:#?}",
                        job_id
                    )));
                }
            }

            Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                set_boot_order_jid: set_boot_order_info.set_boot_order_jid.clone(),
                set_boot_order_state: SetBootOrderState::RebootHost,
                retry_count: set_boot_order_info.retry_count,
            }))
        }
        SetBootOrderState::RebootHost => {
            // Reboot before final verification so any accepted boot-order
            // change or vendor job can apply.
            handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart).await?;

            Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                set_boot_order_jid: set_boot_order_info.set_boot_order_jid.clone(),
                set_boot_order_state: SetBootOrderState::WaitForSetBootOrderJobCompletion,
                retry_count: set_boot_order_info.retry_count,
            }))
        }
        SetBootOrderState::WaitForSetBootOrderJobCompletion => {
            const JOB_QUERY_WAIT_MINUTES: i64 = 5;

            if let Some(job_id) = &set_boot_order_info.set_boot_order_jid {
                let job_state = match redfish_client.get_job_state(job_id).await {
                    Ok(state) => state,
                    Err(e) => {
                        // Wait 5 minutes before declaring the job was lost or failed.
                        // This helps differentiate between transient errors and true failures.
                        let minutes_since_state_change = mh_snapshot
                            .host_snapshot
                            .state
                            .version
                            .since_state_change()
                            .num_minutes();

                        if minutes_since_state_change < JOB_QUERY_WAIT_MINUTES {
                            return Err(redfish_error("get_job_state", e));
                        }

                        tracing::warn!(
                            %job_id,
                            machine_id = %mh_snapshot.host_snapshot.id,
                            time_in_state_minutes = minutes_since_state_change,
                            error = %e,
                            "SetBootOrder: job lookup failed, transitioning to HandleJobFailure"
                        );

                        return Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                            set_boot_order_jid: None,
                            set_boot_order_state: SetBootOrderState::HandleJobFailure {
                                failure: format!("Job {} lookup failed: {}", job_id, e),
                                power_state: PowerState::Off,
                            },
                            retry_count: set_boot_order_info.retry_count,
                        }));
                    }
                };

                match job_state {
                    libredfish::JobState::Completed => {
                        // Job completed successfully, proceed to CheckBootOrder
                    }
                    _ if job_state.is_error_state() => {
                        tracing::warn!(
                            %job_id,
                            machine_id = %mh_snapshot.host_snapshot.id,
                            job_state = ?job_state,
                            "SetBootOrder: job failed, transitioning to HandleJobFailure",
                        );

                        return Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                            set_boot_order_jid: None,
                            set_boot_order_state: SetBootOrderState::HandleJobFailure {
                                failure: format!("Job {} failed: {job_state:#?}", job_id),
                                power_state: PowerState::Off,
                            },
                            retry_count: set_boot_order_info.retry_count,
                        }));
                    }
                    _ => {
                        // Job is still running, wait for completion
                        return Err(StateHandlerError::GenericError(eyre::eyre!(
                            "waiting for job {:#?} to complete; current state: {job_state:#?}",
                            job_id
                        )));
                    }
                }
            }

            Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                set_boot_order_jid: set_boot_order_info.set_boot_order_jid.clone(),
                set_boot_order_state: SetBootOrderState::CheckBootOrder,
                retry_count: set_boot_order_info.retry_count,
            }))
        }
        SetBootOrderState::HandleJobFailure {
            failure,
            power_state,
        } => {
            // Handles recovery when a SetBootOrder BIOS job fails or is lost.
            // 1. Power off the host
            // 2. Reset the BMC
            // 3. Transition to CheckBootOrder to verify and retry if needed

            let current_power_state = redfish_client
                .get_power_state()
                .await
                .map_err(|e| redfish_error("get_power_state", e))?;

            match power_state {
                PowerState::Off => {
                    if current_power_state != libredfish::PowerState::Off {
                        handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceOff)
                            .await?;

                        return Ok(SetBootOrderOutcome::WaitingForReboot(format!(
                            "HandleJobFailure: waiting for {} to power down; current power state: {current_power_state}; failure: {}",
                            mh_snapshot.host_snapshot.id, failure
                        )));
                    }

                    // Host is powered off, reset the BMC
                    tracing::info!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        reason = %failure,
                        "HandleJobFailure: Resetting BMC after failure"
                    );

                    redfish_client
                        .bmc_reset(None)
                        .await
                        .map_err(|e| redfish_error("bmc_reset", e))?;

                    // Transition to PowerState::On to wait for BMC to come back
                    Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                        set_boot_order_jid: None,
                        set_boot_order_state: SetBootOrderState::HandleJobFailure {
                            failure: failure.clone(),
                            power_state: PowerState::On,
                        },
                        retry_count: set_boot_order_info.retry_count,
                    }))
                }
                PowerState::On => {
                    // BMC should be back, power the host back on
                    if current_power_state != libredfish::PowerState::On {
                        // Wait for the BMC to come back online after reset before powering on
                        let basetime = mh_snapshot
                            .host_snapshot
                            .status
                            .last_reboot_requested
                            .as_ref()
                            .map(|x| x.time)
                            .unwrap_or(mh_snapshot.host_snapshot.state.version.timestamp());

                        let power_down_wait = ctx
                            .services
                            .site_config
                            .machine_state_controller
                            .power_down_wait;

                        if Utc::now().signed_duration_since(basetime) < power_down_wait {
                            return Ok(SetBootOrderOutcome::WaitingForReboot(format!(
                                "HandleJobFailure: waiting for BMC to come back online for {}; job failure: {}",
                                mh_snapshot.host_snapshot.id, failure
                            )));
                        }

                        handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::On)
                            .await?;

                        return Ok(SetBootOrderOutcome::WaitingForReboot(format!(
                            "HandleJobFailure: powering on {} after BMC reset; job failure: {}",
                            mh_snapshot.host_snapshot.id, failure
                        )));
                    }

                    // Host is powered on, transition to CheckBootOrder to verify and retry
                    tracing::info!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        "HandleJobFailure: BMC reset complete and host powered on, transitioning to CheckBootOrder",
                    );

                    Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                        set_boot_order_jid: None,
                        set_boot_order_state: SetBootOrderState::CheckBootOrder,
                        retry_count: set_boot_order_info.retry_count,
                    }))
                }
                _ => Err(StateHandlerError::GenericError(eyre::eyre!(
                    "HandleJobFailure: unexpected power state {power_state:#?} for {}",
                    mh_snapshot.host_snapshot.id
                ))),
            }
        }
        SetBootOrderState::CheckBootOrder => {
            const CHECK_BOOT_ORDER_TIMEOUT_MINUTES: i64 = 30;

            let retry_count = set_boot_order_info.retry_count;
            let max_retries = ctx
                .services
                .site_config
                .machine_state_controller
                .max_bios_config_retries;

            let boot_interface = match require_boot_interface(
                &mh_snapshot.host_snapshot.id,
                boot_interface_resolution,
                "verifying boot order",
                SetBootOrderOutcome::Wait,
            )? {
                RequiredBootInterface::Ready(target) => target,
                RequiredBootInterface::Wait(outcome) => return Ok(outcome),
            };

            // The boot-order job/recovery reboot can itself revert the HTTP
            // device or another managed BIOS attribute. Verify BIOS first for
            // every platform and return to the shared job-aware repair before
            // declaring the overall boot configuration complete.
            let is_bios_setup = boot_interface
                .run(|bi| redfish_client.is_bios_setup(Some(bi)))
                .await
                .map_err(|e| redfish_error("is_bios_setup", e))?;

            if !is_bios_setup {
                tracing::warn!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    bmc_vendor = %mh_snapshot.host_snapshot.bmc_vendor(),
                    "Host BIOS drifted during boot-order recovery; returning to shared BIOS repair"
                );
                return Ok(SetBootOrderOutcome::ConfigureBios);
            }

            // A controller upgrade can resume a Viking after an old boot-order
            // job or power-recovery substate. Once the operation and BIOS
            // verification are complete, skip the unsupported order read.
            if should_skip_boot_order_remediation(mh_snapshot) {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    bmc_vendor = %mh_snapshot.host_snapshot.bmc_vendor(),
                    "Skipping boot order verification on Viking (known FW/BMC issue)"
                );
                return Ok(SetBootOrderOutcome::Done);
            }

            let boot_order_configured = boot_interface
                .run(|bi| redfish_client.is_boot_order_setup(bi))
                .await
                .map_err(|e| redfish_error("is_boot_order_setup", e))?;

            if boot_order_configured {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "Boot order verified",
                );
                return Ok(SetBootOrderOutcome::Done);
            }

            // Boot order is not configured properly - check if we should retry
            let time_since_state_change =
                mh_snapshot.host_snapshot.state.version.since_state_change();

            tracing::warn!(
                machine_id = %mh_snapshot.host_snapshot.id,
                retry_count,
                time_in_state_minutes = time_since_state_change.num_minutes(),
                "Boot order check failed after SetBootOrder"
            );

            if time_since_state_change.num_minutes() < CHECK_BOOT_ORDER_TIMEOUT_MINUTES {
                return Err(StateHandlerError::GenericError(eyre::eyre!(
                    "boot order is not configured properly for host {} after SetBootOrder completed (retry_count: {}, max_retries: {}, time_in_state: {} minutes)",
                    mh_snapshot.host_snapshot.id,
                    retry_count,
                    max_retries,
                    time_since_state_change.num_minutes()
                )));
            }

            if retry_count < max_retries {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    time_in_state_minutes = time_since_state_change.num_minutes(),
                    retry_count = retry_count + 1,
                    max_retries,
                    "Boot order check timed out; retrying SetBootOrder"
                );

                return Ok(SetBootOrderOutcome::Continue(SetBootOrderInfo {
                    set_boot_order_jid: None,
                    set_boot_order_state: SetBootOrderState::SetBootOrder,
                    retry_count: retry_count + 1,
                }));
            }

            // Keep the state parked so an operator can repair the BMC and the
            // next verification can complete without resetting the lifecycle.
            Err(StateHandlerError::ManualInterventionRequired(format!(
                "Boot order is not configured properly for host {} after SetBootOrder completed; automated boot-config convergence exhausted (retry_count: {}, max_retries: {}, time_in_state: {} minutes)",
                mh_snapshot.host_snapshot.id,
                retry_count,
                max_retries,
                time_since_state_change.num_minutes()
            )))
        }
    }
}

async fn get_power_state(redfish_client: &dyn Redfish) -> Result<PowerState, StateHandlerError> {
    redfish_client
        .get_power_state()
        .await
        .map_err(|e| redfish_error("get_power_state", e))
        .map(IntoModel::into_model)
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};
    use model::firmware::FirmwareComponent;
    use model::site_explorer::{
        EndpointExplorationReport, EndpointType, Inventory, PreingestionState, Service,
    };
    use regex::Regex;

    use super::*;

    #[test]
    fn pxe_blocking_bgp_alert_requires_probe_and_allocation_classification() {
        check_values(
            [
                Check {
                    scenario: "allocation-blocking ToR alert",
                    input: (
                        HealthProbeId::bgp_peering_tor(),
                        vec![HealthAlertClassification::prevent_allocations()],
                    ),
                    expect: true,
                },
                Check {
                    scenario: "critical ToR alert also blocks PXE",
                    input: (
                        HealthProbeId::bgp_peering_tor(),
                        vec![
                            HealthAlertClassification::prevent_allocations(),
                            HealthAlertClassification::prevent_host_state_changes(),
                        ],
                    ),
                    expect: true,
                },
                Check {
                    scenario: "visible ToR alert does not block PXE",
                    input: (HealthProbeId::bgp_peering_tor(), vec![]),
                    expect: false,
                },
                Check {
                    scenario: "host-state classification alone is not the p0 signal",
                    input: (
                        HealthProbeId::bgp_peering_tor(),
                        vec![HealthAlertClassification::prevent_host_state_changes()],
                    ),
                    expect: false,
                },
                Check {
                    scenario: "different allocation-blocking probe",
                    input: (
                        HealthProbeId::from_str("BgpPeeringRouteServer")
                            .expect("valid test probe id"),
                        vec![HealthAlertClassification::prevent_allocations()],
                    ),
                    expect: false,
                },
            ],
            |(id, classifications)| {
                is_pxe_blocking_bgp_alert(&HealthProbeAlert {
                    id,
                    target: None,
                    in_alert_since: None,
                    message: "test alert".to_string(),
                    tenant_message: None,
                    classifications,
                })
            },
        );
    }

    #[test]
    fn release_pxe_waits_only_for_relevant_network_alerts() {
        fn report(alerts: Vec<HealthProbeAlert>) -> HealthReport {
            HealthReport {
                source: HealthReport::DPU_AGENT_SOURCE.to_string(),
                triggered_by: None,
                observed_at: None,
                successes: vec![],
                alerts,
            }
        }

        fn alert(
            probe_id: HealthProbeId,
            classifications: Vec<HealthAlertClassification>,
        ) -> HealthProbeAlert {
            HealthProbeAlert {
                id: probe_id,
                target: None,
                in_alert_since: None,
                message: "test alert".to_string(),
                tenant_message: None,
                classifications,
            }
        }

        fn allocation_blocking_alert(probe_id: HealthProbeId) -> HealthProbeAlert {
            alert(
                probe_id,
                vec![HealthAlertClassification::prevent_allocations()],
            )
        }

        check_values(
            [
                Check {
                    scenario: "no network alert",
                    input: report(vec![]),
                    expect: false,
                },
                Check {
                    scenario: "NVUE API is unavailable",
                    input: report(vec![allocation_blocking_alert(
                        HealthProbeId::nvue_api_running(),
                    )]),
                    expect: true,
                },
                Check {
                    scenario: "FRR BGP health check failed",
                    input: report(vec![allocation_blocking_alert(HealthProbeId::bgp_stats())]),
                    expect: true,
                },
                Check {
                    scenario: "network config is still settling",
                    input: report(vec![allocation_blocking_alert(
                        HealthProbeId::post_config_check_wait(),
                    )]),
                    expect: true,
                },
                Check {
                    scenario: "informational NVUE alert does not block release",
                    input: report(vec![alert(HealthProbeId::nvue_api_running(), vec![])]),
                    expect: false,
                },
                Check {
                    scenario: "unrelated critical alert does not block release",
                    input: report(vec![alert(
                        HealthProbeId::from_str("DpuDiskUtilizationCritical")
                            .expect("valid test probe id"),
                        vec![HealthAlertClassification::prevent_host_state_changes()],
                    )]),
                    expect: false,
                },
            ],
            |report| report_has_release_pxe_blocking_network_alert(&report),
        );
    }

    #[test]
    fn dpu_restart_requires_a_stable_power_state() {
        check_values(
            [
                Check {
                    scenario: "powered-off DPU is powered on",
                    input: libredfish::PowerState::Off,
                    expect: Ok(SystemPowerControl::On),
                },
                Check {
                    scenario: "powered-on DPU is restarted",
                    input: libredfish::PowerState::On,
                    expect: Ok(SystemPowerControl::ForceRestart),
                },
                Check {
                    scenario: "DPU that is powering off is retried",
                    input: libredfish::PowerState::PoweringOff,
                    expect: Err(()),
                },
                Check {
                    scenario: "DPU that is powering on is retried",
                    input: libredfish::PowerState::PoweringOn,
                    expect: Err(()),
                },
                Check {
                    scenario: "paused DPU is powered on",
                    input: libredfish::PowerState::Paused,
                    expect: Ok(SystemPowerControl::On),
                },
                Check {
                    scenario: "resetting DPU is retried",
                    input: libredfish::PowerState::Reset,
                    expect: Err(()),
                },
                Check {
                    scenario: "DPU with unknown power state is retried",
                    input: libredfish::PowerState::Unknown,
                    expect: Err(()),
                },
            ],
            |power_state| dpu_restart_power_action(power_state).map_err(|_| ()),
        );
    }

    #[test]
    fn terminal_ready_boot_config_failure_is_deferred_until_lockdown_restoration() {
        let desired_version = ConfigVersion::initial();
        let desired_boot_interface =
            MachineBootInterfaceTarget::MacOnly("02:00:00:00:00:01".parse().unwrap());
        let failure = "BIOS job retries exhausted".to_string();

        assert_eq!(
            ready_boot_config_locking(
                Versioned::new(desired_boot_interface.clone(), desired_version),
                0,
                Some(ReadyBootConfigPostLockAction::Convergence {
                    failure: failure.clone(),
                }),
            ),
            ManagedHostState::BootConfiguring {
                desired_version,
                desired_boot_interface,
                post_lock_verification_retry_count: 0,
                boot_config_state: ReadyBootConfigState::LockHost {
                    post_lock_action: Some(ReadyBootConfigPostLockAction::Convergence { failure }),
                },
            }
        );
    }

    #[test]
    fn only_ready_boot_config_handles_missing_dpu_snapshots() {
        let desired = Versioned::new(
            MachineBootInterfaceTarget::MacOnly("02:00:00:00:00:01".parse().unwrap()),
            ConfigVersion::initial(),
        );

        assert!(ready_boot_config_handles_missing_dpu_snapshots(
            &ready_boot_configuring(desired, 0, ReadyBootConfigState::Prepare),
        ));
        assert!(!ready_boot_config_handles_missing_dpu_snapshots(
            &ManagedHostState::Ready,
        ));
    }

    #[test]
    fn stalled_ready_boot_config_cleans_up_only_after_work_can_start() {
        let timed_out = chrono::Duration::from_std(model::machine::slas::BOOT_CONFIGURING).unwrap();
        let still_within_sla = timed_out - chrono::Duration::seconds(1);

        assert!(!ready_boot_config_requires_timeout_cleanup(
            &ReadyBootConfigState::CheckHostConfig,
            still_within_sla,
        ));
        assert!(ready_boot_config_requires_timeout_cleanup(
            &ReadyBootConfigState::CheckHostConfig,
            timed_out,
        ));

        for safe_state in [
            ReadyBootConfigState::Prepare,
            ReadyBootConfigState::LockHost {
                post_lock_action: None,
            },
            ReadyBootConfigState::Failed {
                failure: "already parked".to_string(),
            },
        ] {
            assert!(!ready_boot_config_requires_timeout_cleanup(
                &safe_state,
                timed_out,
            ));
        }
    }

    #[test]
    fn post_lock_drift_retries_once_then_requires_intervention() {
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .unwrap();
        let desired_version = ConfigVersion::initial();
        let desired_boot_interface =
            MachineBootInterfaceTarget::MacOnly("02:00:00:00:00:01".parse().unwrap());
        let desired = Versioned::new(desired_boot_interface.clone(), desired_version);

        assert_eq!(
            ready_boot_config_after_post_lock_drift(desired.clone(), 0, machine_id),
            ManagedHostState::BootConfiguring {
                desired_version,
                desired_boot_interface: desired_boot_interface.clone(),
                post_lock_verification_retry_count: 1,
                boot_config_state: ReadyBootConfigState::Prepare,
            }
        );

        assert!(matches!(
            ready_boot_config_after_post_lock_drift(desired, 1, machine_id),
            ManagedHostState::BootConfiguring {
                desired_version: version,
                desired_boot_interface: target,
                post_lock_verification_retry_count: 1,
                boot_config_state: ReadyBootConfigState::Failed { failure },
            } if version == desired_version
                && target == desired_boot_interface
                && failure.contains("repeatedly drifted")
        ));
    }

    #[test]
    fn ready_boot_config_adopts_targets_only_at_safe_boundaries() {
        let set_boot_order = |set_boot_order_state| ReadyBootConfigState::SetBootOrder {
            set_boot_order_info: SetBootOrderInfo {
                set_boot_order_jid: None,
                set_boot_order_state,
                retry_count: 0,
            },
        };

        for state in [
            ReadyBootConfigState::Prepare,
            ReadyBootConfigState::CheckHostConfig,
            ReadyBootConfigState::ConfigureBios { retry_count: 0 },
            set_boot_order(SetBootOrderState::SetBootOrder),
            set_boot_order(SetBootOrderState::CheckBootOrder),
        ] {
            assert!(ready_boot_config_can_adopt_latest(&state), "{state:?}");
        }

        for state in [
            ReadyBootConfigState::UnlockHost {
                unlock_host_state: UnlockHostState::RebootHost,
            },
            ReadyBootConfigState::PollingBiosSetup { retry_count: 0 },
            set_boot_order(SetBootOrderState::WaitForSetBootOrderJobCompletion),
            ReadyBootConfigState::LockHost {
                post_lock_action: Some(ReadyBootConfigPostLockAction::Convergence {
                    failure: "exhausted".to_string(),
                }),
            },
            ReadyBootConfigState::Failed {
                failure: "exhausted".to_string(),
            },
        ] {
            assert!(!ready_boot_config_can_adopt_latest(&state), "{state:?}");
        }
    }

    #[test]
    fn failed_ready_boot_config_restarts_only_for_superseding_intent() {
        let failed_version = ConfigVersion::initial();
        let next_version = failed_version.increment();
        let failed_target =
            MachineBootInterfaceTarget::MacOnly("02:00:00:00:00:01".parse().unwrap());
        let next_target = MachineBootInterfaceTarget::MacOnly("02:00:00:00:00:02".parse().unwrap());

        assert_eq!(
            ready_boot_config_superseded_state(
                failed_version,
                Some(Versioned::new(failed_target, failed_version)),
            ),
            None,
        );
        assert_eq!(
            ready_boot_config_superseded_state(
                failed_version,
                Some(Versioned::new(next_target.clone(), next_version)),
            ),
            Some(ManagedHostState::BootConfiguring {
                desired_version: next_version,
                desired_boot_interface: next_target,
                post_lock_verification_retry_count: 0,
                boot_config_state: ReadyBootConfigState::Prepare,
            }),
        );
        assert_eq!(
            ready_boot_config_superseded_state(failed_version, None),
            Some(ManagedHostState::Ready),
        );
    }

    #[test]
    fn ipmi_socket_address_uses_reported_or_default_port() {
        let ip_address = IpAddr::V4("192.0.2.10".parse().unwrap());

        assert_eq!(
            ipmi_socket_address(ip_address, Some(1623)),
            "192.0.2.10:1623".parse().unwrap(),
        );
        assert_eq!(
            ipmi_socket_address(ip_address, None),
            "192.0.2.10:623".parse().unwrap(),
        );
    }

    /// One emit per actual retry: the INFO line carries the machine, the
    /// 1-based attempt, and the failure it recovers from, and the unlabeled
    /// counter moves by one.
    #[test]
    fn host_firmware_upgrade_retry_logs_and_counts() {
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .unwrap();

        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            carbide_instrument::emit(HostFirmwareUpgradeRetried {
                machine_id,
                attempt: 1,
                error: "scout upgrade failed".to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::INFO);
        assert_eq!(logs[0].message, "Retrying the host firmware upgrade");
        let field = |name: &str| {
            logs[0]
                .fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(field("machine_id"), Some(machine_id.to_string()));
        assert_eq!(field("attempt"), Some("1".to_string()));
        assert_eq!(field("error"), Some("scout upgrade failed".to_string()));
        assert_eq!(
            metrics.counter_delta("carbide_host_reprovision_retries_total", &[]),
            1.0
        );
    }

    #[test]
    fn scout_firmware_upgrade_deadline_accounts_for_each_artifact() {
        let started_at = chrono::DateTime::<Utc>::from_str("2026-04-28T00:00:00Z").unwrap();

        let deadline = scout_firmware_upgrade_deadline(started_at, 300, 120, 3);

        assert_eq!(
            deadline,
            started_at
                + Duration::seconds(30)
                + Duration::seconds(300)
                + Duration::seconds(120 * 3)
                + Duration::minutes(30)
        );
    }

    #[test]
    fn scout_firmware_upgrade_deadline_is_capped() {
        let started_at = chrono::DateTime::<Utc>::from_str("2026-04-28T00:00:00Z").unwrap();

        let deadline = scout_firmware_upgrade_deadline(started_at, u32::MAX, u32::MAX, usize::MAX);

        assert_eq!(deadline, started_at + Duration::hours(5));
    }

    #[test]
    fn need_host_fw_upgrade_checks_all_matching_cx7_inventories() {
        let firmware_type = FirmwareComponentType::Cx7;
        let target_version = "28.47.2682";
        let old_version = "28.46.1000";
        let fw_info = Firmware {
            vendor: bmc_vendor::BMCVendor::Nvidia,
            model: "DGXH100".to_string(),
            components: HashMap::from([(
                firmware_type,
                FirmwareComponent {
                    current_version_reported_as: Some(Regex::new(r"^CX7_[0-9]+$").unwrap()),
                    preingest_upgrade_when_below: None,
                    known_firmware: vec![FirmwareEntry::standard_filename(
                        target_version,
                        "/opt/carbide/firmware/cx7.bin",
                    )],
                },
            )]),
            explicit_start_needed: false,
            ordering: vec![firmware_type],
        };
        let endpoint = ExploredEndpoint {
            address: "192.0.2.10".parse().unwrap(),
            report: EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                service: vec![Service {
                    id: "FirmwareInventory".to_string(),
                    inventories: vec![
                        Inventory {
                            id: "CX7_0".to_string(),
                            description: None,
                            version: Some(target_version.to_string()),
                            release_date: None,
                        },
                        Inventory {
                            id: "CX7_1".to_string(),
                            description: None,
                            version: Some(old_version.to_string()),
                            release_date: None,
                        },
                    ],
                }],
                versions: HashMap::from([(firmware_type, target_version.to_string())]),
                ..Default::default()
            },
            report_version: ConfigVersion::new(1),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_ingestion_and_poweron: false,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
        };

        let to_install = need_host_fw_upgrade(&endpoint, &fw_info, firmware_type)
            .expect("stale CX7 inventory should require upgrade");

        assert_eq!(to_install.version, target_version);
    }

    #[test]
    fn handle_no_dpu_error_treats_as_ok_on_zero_dpu_host() {
        let result = handle_no_dpu_error(Err(RedfishError::NoDpu), 0, "machine_setup");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn handle_no_dpu_error_surfaces_when_dpus_were_expected() {
        let result = handle_no_dpu_error(Err(RedfishError::NoDpu), 2, "machine_setup");
        assert!(matches!(result, Err(RedfishError::NoDpu)));
    }

    #[test]
    fn handle_no_dpu_error_passes_through_success() {
        let job_id = "bios-job-1".to_string();
        let result = handle_no_dpu_error(Ok(Some(job_id.clone())), 0, "machine_setup");
        assert!(matches!(result, Ok(Some(ref s)) if s == &job_id));

        let result = handle_no_dpu_error(Ok(None), 2, "machine_setup");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn handle_no_dpu_error_does_not_touch_other_errors() {
        // Other error variants must surface, even on zero-DPU hosts -- we
        // only ignore the *specific* NoDpu signal.
        let result =
            handle_no_dpu_error(Err(RedfishError::Lockdown), 0, "set_boot_order_dpu_first");
        assert!(matches!(result, Err(RedfishError::Lockdown)));
    }

    #[test]
    fn test_cycle_1() {
        let state_change_time =
            chrono::DateTime::<Utc>::from_str("2024-01-30T11:26:18.261228950+00:00").unwrap();

        let expected_time = state_change_time + Duration::minutes(30);
        let wait_period = Duration::minutes(30);

        let cycle = get_reboot_cycle(expected_time, state_change_time, wait_period).unwrap();
        assert_eq!(cycle, 1);
    }

    #[test]
    fn test_cycle_2() {
        let state_change_time =
            chrono::DateTime::<Utc>::from_str("2024-01-30T11:26:18.261228950+00:00").unwrap();

        let expected_time = state_change_time + Duration::minutes(70);
        let wait_period = Duration::minutes(30);

        let cycle = get_reboot_cycle(expected_time, state_change_time, wait_period).unwrap();
        assert_eq!(cycle, 2);
    }

    #[test]
    fn test_cycle_3() {
        let state_change_time =
            chrono::DateTime::<Utc>::from_str("2024-01-30T11:26:18.261228950+00:00").unwrap();

        let expected_time = state_change_time + Duration::minutes(121);
        let wait_period = Duration::minutes(30);

        let cycle = get_reboot_cycle(expected_time, state_change_time, wait_period).unwrap();
        assert_eq!(cycle, 4);
    }

    #[test]
    fn test_cycle_4() {
        let state_change_time =
            chrono::DateTime::<Utc>::from_str("2024-01-30T11:26:18.261228950+00:00").unwrap();

        let expected_time = state_change_time + Duration::minutes(30);
        let wait_period = Duration::minutes(0);

        let cycle = get_reboot_cycle(expected_time, state_change_time, wait_period).unwrap();
        assert_eq!(cycle, 30);
    }

    #[test]
    fn is_reprovision_restartable_failure_matches_expected_causes() {
        let host_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .unwrap();
        let dpu_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap();

        let make_failed = |cause: FailureCause, machine_id: MachineId| ManagedHostState::Failed {
            details: FailureDetails {
                cause,
                failed_at: chrono::Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
            },
            machine_id,
            retry_count: 0,
        };

        // BiosSetupFailed on host → restartable
        assert!(is_reprovision_restartable_failure(
            &make_failed(
                FailureCause::BiosSetupFailed { err: "bios".into() },
                host_id
            ),
            &host_id,
        ));

        // DpfProvisioning on host → restartable
        assert!(is_reprovision_restartable_failure(
            &make_failed(FailureCause::DpfProvisioning { err: "dpf".into() }, host_id),
            &host_id,
        ));

        // DpfProvisioning attributed to a DPU → still restartable (no machine_id guard)
        assert!(is_reprovision_restartable_failure(
            &make_failed(FailureCause::DpfProvisioning { err: "dpf".into() }, dpu_id),
            &host_id,
        ));

        // DpfProvisioning on host with any FailureSource → restartable
        assert!(is_reprovision_restartable_failure(
            &ManagedHostState::Failed {
                details: FailureDetails {
                    cause: FailureCause::DpfProvisioning { err: "dpf".into() },
                    failed_at: chrono::Utc::now(),
                    source: FailureSource::StateMachineArea(StateMachineArea::HostInit),
                },
                machine_id: host_id,
                retry_count: 0,
            },
            &host_id,
        ));
    }

    mod is_bf4_dmi_product_tests {
        use super::*;

        #[test]
        fn bf4_product_name_is_detected() {
            assert!(is_bf4_dmi_product("BlueField-4 SmartNIC Main Card"));
        }

        #[test]
        fn bf4_detection_is_case_insensitive() {
            assert!(is_bf4_dmi_product("bluefield-4 smartnic main card"));
        }

        #[test]
        fn bf3_product_name_is_not_bf4() {
            assert!(!is_bf4_dmi_product("BlueField SoC"));
        }

        #[test]
        fn empty_product_name_is_not_bf4() {
            assert!(!is_bf4_dmi_product(""));
        }
    }

    mod is_dpf_managed_bf4_tests {
        use model::hardware_info::{DmiData, HardwareInfo};
        use model::test_support::machine_snapshot::{dpu_machine, managed_host_state_snapshot};

        use super::*;

        fn dpu_with_product_name(product_name: &str) -> Machine {
            let mut dpu = dpu_machine(0);
            dpu.status.hardware_info = Some(HardwareInfo {
                dmi_data: Some(DmiData {
                    product_name: product_name.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            });
            dpu
        }

        fn state_with_ingestion(used_for_ingestion: bool) -> ManagedHostStateSnapshot {
            let mut state = managed_host_state_snapshot();
            state.host_snapshot.config.dpf.used_for_ingestion = used_for_ingestion;
            state
        }

        #[test]
        fn returns_false_when_dpf_not_used_for_ingestion() {
            let state = state_with_ingestion(false);
            let dpu = dpu_with_product_name("BlueField-4 SmartNIC Main Card");
            assert!(!is_dpf_managed_bf4(&state, &dpu).unwrap());
        }

        #[test]
        fn returns_true_for_bf4_product_name() {
            let state = state_with_ingestion(true);
            let dpu = dpu_with_product_name("BlueField-4 SmartNIC Main Card");
            assert!(is_dpf_managed_bf4(&state, &dpu).unwrap());
        }

        #[test]
        fn returns_false_for_bf3_product_name() {
            let state = state_with_ingestion(true);
            let dpu = dpu_with_product_name("BlueField-3 Dpu");
            assert!(!is_dpf_managed_bf4(&state, &dpu).unwrap());
        }

        #[test]
        fn errors_when_dmi_data_absent() {
            let state = state_with_ingestion(true);
            let mut dpu = dpu_machine(0);
            dpu.status.hardware_info = Some(HardwareInfo::default());
            assert!(matches!(
                is_dpf_managed_bf4(&state, &dpu),
                Err(StateHandlerError::MissingData { .. })
            ));
        }

        #[test]
        fn errors_when_product_name_empty() {
            let state = state_with_ingestion(true);
            let dpu = dpu_with_product_name("");
            assert!(matches!(
                is_dpf_managed_bf4(&state, &dpu),
                Err(StateHandlerError::MissingData { .. })
            ));
        }

        #[test]
        fn errors_when_product_name_whitespace_only() {
            let state = state_with_ingestion(true);
            let dpu = dpu_with_product_name("   ");
            assert!(matches!(
                is_dpf_managed_bf4(&state, &dpu),
                Err(StateHandlerError::MissingData { .. })
            ));
        }
    }
}
