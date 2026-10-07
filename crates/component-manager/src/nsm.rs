// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use carbide_instrument::red;
use mac_address::MacAddress;
use model::component_manager::{
    ConfigureSwitchCertificateState, FirmwareState, NvSwitchComponent, PowerAction,
};
use tonic::transport::Channel;
use trace_propagation::TraceInjectService;
use tracing::instrument;

use crate::config::BackendTlsConfig;
use crate::error::ComponentManagerError;
use crate::nv_switch_manager::{
    ConfigureSwitchCertificateJobStatus, NvSwitchManager, SwitchComponentResult, SwitchEndpoint,
    SwitchFirmwareUpdateStatus, SwitchPowerStateResult, SwitchSlotAndTrayResult,
};
use crate::proto::nsm;
use crate::types::parse_mac;

#[derive(Debug)]
pub struct NsmSwitchBackend {
    client: nsm::nv_switch_manager_client::NvSwitchManagerClient<TraceInjectService<Channel>>,
}

impl NsmSwitchBackend {
    pub const BACKEND_NAME: &str = "nsm";
    const PASSTHROUGH_CERT_JOB_ID: &str = "nsm-switch-cert-passthrough";

    pub async fn connect(
        url: &str,
        tls: Option<&BackendTlsConfig>,
    ) -> Result<Self, ComponentManagerError> {
        let channel = crate::tls::build_channel(url, tls, "NSM").await?;
        Ok(Self {
            client: nsm::nv_switch_manager_client::NvSwitchManagerClient::new(channel),
        })
    }
}

fn to_nsm_component(c: &NvSwitchComponent) -> nsm::NvSwitchComponent {
    match c {
        NvSwitchComponent::Bmc => nsm::NvSwitchComponent::NvswitchComponentBmc,
        NvSwitchComponent::Cpld => nsm::NvSwitchComponent::NvswitchComponentCpld,
        NvSwitchComponent::Bios => nsm::NvSwitchComponent::NvswitchComponentBios,
        NvSwitchComponent::Nvos => nsm::NvSwitchComponent::NvswitchComponentNvos,
    }
}

fn map_nsm_update_state(state: i32) -> FirmwareState {
    match nsm::UpdateState::try_from(state) {
        Ok(nsm::UpdateState::Queued) => FirmwareState::Queued,
        Ok(nsm::UpdateState::Copy)
        | Ok(nsm::UpdateState::Upload)
        | Ok(nsm::UpdateState::Install)
        | Ok(nsm::UpdateState::PollCompletion)
        | Ok(nsm::UpdateState::PowerCycle)
        | Ok(nsm::UpdateState::WaitReachable) => FirmwareState::InProgress,
        Ok(nsm::UpdateState::Verify) | Ok(nsm::UpdateState::Cleanup) => FirmwareState::Verifying,
        Ok(nsm::UpdateState::Completed) => FirmwareState::Completed,
        Ok(nsm::UpdateState::Failed) => FirmwareState::Failed,
        Ok(nsm::UpdateState::Cancelled) => FirmwareState::Cancelled,
        _ => FirmwareState::Unknown,
    }
}

fn credentials_to_nsm(creds: &carbide_secrets::credentials::Credentials) -> nsm::Credentials {
    match creds {
        carbide_secrets::credentials::Credentials::UsernamePassword { username, password } => {
            nsm::Credentials {
                username: username.clone(),
                password: password.clone(),
            }
        }
    }
}

/// Builds a single registration request for an endpoint.
fn build_registration(ep: &SwitchEndpoint) -> nsm::RegisterNvSwitchRequest {
    nsm::RegisterNvSwitchRequest {
        vendor: nsm::Vendor::Nvidia as i32,
        bmc: Some(nsm::Subsystem {
            mac_address: ep.bmc_mac.to_string(),
            ip_address: ep.bmc_ip.to_string(),
            credentials: Some(credentials_to_nsm(&ep.bmc_credentials)),
            port: 0,
        }),
        nvos: Some(nsm::Subsystem {
            mac_address: ep.nvos_mac.to_string(),
            ip_address: ep.nvos_ip.to_string(),
            credentials: Some(credentials_to_nsm(&ep.nvos_credentials)),
            port: 0,
        }),
        rack_id: String::new(),
    }
}

/// Registers endpoints with NSM one at a time and returns bidirectional
/// maps between BMC MAC and NSM-generated UUID.
///
/// Each endpoint is registered individually to avoid relying on response
/// ordering from the batch API. This can be switched back to batch
/// registration once NSM includes a correlation key (e.g. BMC MAC) in
/// RegisterNVSwitchResponse.
async fn register_and_map(
    client: &mut nsm::nv_switch_manager_client::NvSwitchManagerClient<TraceInjectService<Channel>>,
    endpoints: &[SwitchEndpoint],
) -> Result<(HashMap<MacAddress, String>, HashMap<String, MacAddress>), ComponentManagerError> {
    let mut mac_to_uuid: HashMap<MacAddress, String> = HashMap::new();
    let mut uuid_to_mac: HashMap<String, MacAddress> = HashMap::new();

    for ep in endpoints {
        let req = build_registration(ep);
        let response = red::instrumented(
            "nsm",
            "register_nv_switches",
            client.register_nv_switches(nsm::RegisterNvSwitchesRequest {
                registration_requests: vec![req],
            }),
        )
        .await?
        .into_inner();

        let Some(reg_resp) = response.responses.into_iter().next() else {
            tracing::warn!(bmc_mac_address = %ep.bmc_mac, "NSM returned empty response for switch");
            continue;
        };

        if reg_resp.status != nsm::StatusCode::Success as i32 {
            tracing::warn!(
                bmc_mac_address = %ep.bmc_mac,
                error = %reg_resp.error,
                "NSM registration failed for switch"
            );
            continue;
        }

        mac_to_uuid.insert(ep.bmc_mac, reg_resp.uuid.clone());
        uuid_to_mac.insert(reg_resp.uuid.clone(), ep.bmc_mac);
    }

    if mac_to_uuid.is_empty() && !endpoints.is_empty() {
        return Err(ComponentManagerError::Internal(
            "NSM registration failed for all switches".into(),
        ));
    }

    Ok((mac_to_uuid, uuid_to_mac))
}

#[async_trait::async_trait]
impl NvSwitchManager for NsmSwitchBackend {
    fn name(&self) -> &str {
        Self::BACKEND_NAME
    }

    #[instrument(skip(self), fields(backend = "nsm"))]
    async fn power_control(
        &self,
        endpoints: &[SwitchEndpoint],
        action: PowerAction,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        let (mac_to_uuid, uuid_to_mac) =
            register_and_map(&mut self.client.clone(), endpoints).await?;

        let mut results: Vec<SwitchComponentResult> = Vec::new();
        let mut uuids: Vec<String> = Vec::new();

        for ep in endpoints {
            match mac_to_uuid.get(&ep.bmc_mac) {
                Some(uuid) => uuids.push(uuid.clone()),
                None => results.push(SwitchComponentResult {
                    bmc_mac: ep.bmc_mac,
                    success: false,
                    error: Some("NSM registration failed for switch".into()),
                }),
            }
        }

        let nsm_action = match action {
            PowerAction::On => nsm::PowerAction::On,
            PowerAction::GracefulShutdown => nsm::PowerAction::GracefulShutdown,
            PowerAction::ForceOff => nsm::PowerAction::ForceOff,
            PowerAction::GracefulRestart => nsm::PowerAction::GracefulRestart,
            PowerAction::ForceRestart => nsm::PowerAction::ForceRestart,
            PowerAction::AcPowercycle => nsm::PowerAction::PowerCycle,
        };

        if !uuids.is_empty() {
            let request = nsm::PowerControlRequest {
                uuids,
                action: nsm_action as i32,
            };

            let response = red::instrumented(
                "nsm",
                "power_control",
                self.client.clone().power_control(request),
            )
            .await?
            .into_inner();

            for r in response.responses {
                let bmc_mac = uuid_to_mac
                    .get(&r.uuid)
                    .copied()
                    .map(Ok)
                    .unwrap_or_else(|| parse_mac(&r.uuid))?;
                results.push(SwitchComponentResult {
                    bmc_mac,
                    success: r.status == nsm::StatusCode::Success as i32,
                    error: if r.error.is_empty() {
                        None
                    } else {
                        Some(r.error)
                    },
                });
            }
        }

        Ok(results)
    }

    #[instrument(skip(self), fields(backend = "nsm"))]
    async fn queue_firmware_updates(
        &self,
        endpoints: &[SwitchEndpoint],
        bundle_version: &str,
        components: &[NvSwitchComponent],
        _options: &crate::types::FirmwareUpdateOptions,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        let (mac_to_uuid, uuid_to_mac) =
            register_and_map(&mut self.client.clone(), endpoints).await?;

        let mut results: Vec<SwitchComponentResult> = Vec::new();
        let mut uuids: Vec<String> = Vec::new();

        for ep in endpoints {
            match mac_to_uuid.get(&ep.bmc_mac) {
                Some(uuid) => uuids.push(uuid.clone()),
                None => results.push(SwitchComponentResult {
                    bmc_mac: ep.bmc_mac,
                    success: false,
                    error: Some("NSM registration failed for switch".into()),
                }),
            }
        }

        if !uuids.is_empty() {
            let nsm_components: Vec<i32> = components
                .iter()
                .map(|c| to_nsm_component(c) as i32)
                .collect();

            let request = nsm::QueueUpdatesRequest {
                switch_uuids: uuids,
                bundle_version: bundle_version.to_owned(),
                components: nsm_components,
            };

            let response = red::instrumented(
                "nsm",
                "queue_updates",
                self.client.clone().queue_updates(request),
            )
            .await?
            .into_inner();

            for r in response.results {
                let bmc_mac = uuid_to_mac
                    .get(&r.switch_uuid)
                    .copied()
                    .map(Ok)
                    .unwrap_or_else(|| parse_mac(&r.switch_uuid))?;
                results.push(SwitchComponentResult {
                    bmc_mac,
                    success: r.status == nsm::StatusCode::Success as i32,
                    error: if r.error.is_empty() {
                        None
                    } else {
                        Some(r.error)
                    },
                });
            }
        }

        Ok(results)
    }

    #[instrument(skip(self), fields(backend = "nsm"))]
    async fn get_firmware_status(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchFirmwareUpdateStatus>, ComponentManagerError> {
        let (mac_to_uuid, uuid_to_mac) =
            register_and_map(&mut self.client.clone(), endpoints).await?;

        let mut statuses = Vec::new();

        for ep in endpoints {
            let Some(uuid) = mac_to_uuid.get(&ep.bmc_mac) else {
                statuses.push(SwitchFirmwareUpdateStatus {
                    bmc_mac: ep.bmc_mac,
                    state: FirmwareState::Unknown,
                    target_version: String::new(),
                    error: Some("NSM registration failed for switch".into()),
                });
                continue;
            };

            let request = nsm::GetUpdatesForSwitchRequest {
                switch_uuid: uuid.clone(),
            };
            let response = red::instrumented(
                "nsm",
                "get_updates_for_switch",
                self.client.clone().get_updates_for_switch(request),
            )
            .await?
            .into_inner();

            for update in response.updates {
                let bmc_mac = uuid_to_mac
                    .get(&update.switch_uuid)
                    .copied()
                    .unwrap_or(ep.bmc_mac);
                statuses.push(SwitchFirmwareUpdateStatus {
                    bmc_mac,
                    state: map_nsm_update_state(update.state),
                    target_version: update.version_to,
                    error: if update.error_message.is_empty() {
                        None
                    } else {
                        Some(update.error_message)
                    },
                });
            }
        }
        Ok(statuses)
    }

    #[instrument(skip(self), fields(backend = "nsm"))]
    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        let response =
            red::instrumented("nsm", "list_bundles", self.client.clone().list_bundles(()))
                .await?
                .into_inner();

        Ok(response.bundles.into_iter().map(|b| b.version).collect())
    }

    #[instrument(skip(self), fields(backend = "nsm"))]
    async fn get_slot_and_tray(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchSlotAndTrayResult>, ComponentManagerError> {
        tracing::warn!("slot and tray lookup is not supported by NSM backend, passthrough");
        Ok(endpoints
            .iter()
            .map(|ep| SwitchSlotAndTrayResult {
                bmc_mac: ep.bmc_mac,
                slot_number: None,
                tray_index: None,
                error: None,
            })
            .collect())
    }

    #[instrument(skip(self), fields(backend = "nsm"))]
    async fn get_power_state(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchPowerStateResult>, ComponentManagerError> {
        tracing::warn!("get power state is not supported by NSM backend, passthrough");
        Ok(endpoints
            .iter()
            .map(|ep| SwitchPowerStateResult {
                bmc_mac: ep.bmc_mac,
                power_state: None,
                error: None,
            })
            .collect())
    }

    async fn configure_switch_certificate(
        &self,
        endpoint: &SwitchEndpoint,
        domain_name: Option<&str>,
        services: Option<&[i32]>,
    ) -> Result<String, ComponentManagerError> {
        tracing::warn!(
            bmc_mac_address = %endpoint.bmc_mac,
            ?domain_name,
            ?services,
            "switch certificate configuration is not supported by NSM backend, passthrough"
        );
        Ok(Self::PASSTHROUGH_CERT_JOB_ID.to_string())
    }

    async fn get_configure_switch_certificate_job_status(
        &self,
        job_id: &str,
    ) -> Result<ConfigureSwitchCertificateJobStatus, ComponentManagerError> {
        tracing::warn!(
            %job_id,
            "switch certificate job status is not supported by NSM backend, passthrough"
        );
        Ok(ConfigureSwitchCertificateJobStatus {
            state: ConfigureSwitchCertificateState::Completed,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_secrets::credentials::Credentials;
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn nsm_update_state_maps_each_variant() {
        value_scenarios!(run = |state: nsm::UpdateState| map_nsm_update_state(state as i32);
            "queued" {
                nsm::UpdateState::Queued => FirmwareState::Queued,
            }

            "in progress" {
                nsm::UpdateState::Copy => FirmwareState::InProgress,
                nsm::UpdateState::Upload => FirmwareState::InProgress,
                nsm::UpdateState::Install => FirmwareState::InProgress,
                nsm::UpdateState::PollCompletion => FirmwareState::InProgress,
                nsm::UpdateState::PowerCycle => FirmwareState::InProgress,
                nsm::UpdateState::WaitReachable => FirmwareState::InProgress,
            }

            "verifying" {
                nsm::UpdateState::Verify => FirmwareState::Verifying,
                nsm::UpdateState::Cleanup => FirmwareState::Verifying,
            }

            "completed" {
                nsm::UpdateState::Completed => FirmwareState::Completed,
            }

            "failed" {
                nsm::UpdateState::Failed => FirmwareState::Failed,
            }

            "cancelled" {
                nsm::UpdateState::Cancelled => FirmwareState::Cancelled,
            }
        );
    }

    #[test]
    fn nsm_update_state_unknown_for_unrecognized_value() {
        value_scenarios!(map_nsm_update_state:
            "unrecognized" {
                9999 => FirmwareState::Unknown,
            }
        );
    }

    #[test]
    fn to_nsm_component_maps_each_variant() {
        value_scenarios!(run = |component: NvSwitchComponent| to_nsm_component(&component);
            "components" {
                NvSwitchComponent::Bmc => nsm::NvSwitchComponent::NvswitchComponentBmc,
                NvSwitchComponent::Cpld => nsm::NvSwitchComponent::NvswitchComponentCpld,
                NvSwitchComponent::Bios => nsm::NvSwitchComponent::NvswitchComponentBios,
                NvSwitchComponent::Nvos => nsm::NvSwitchComponent::NvswitchComponentNvos,
            }
        );
    }

    #[test]
    fn build_registration_populates_fields() {
        let ep = SwitchEndpoint {
            bmc_ip: "10.0.0.1".parse().unwrap(),
            bmc_mac: "AA:BB:CC:DD:EE:01".parse().unwrap(),
            nvos_ip: "10.0.0.2".parse().unwrap(),
            nvos_mac: "AA:BB:CC:DD:EE:02".parse().unwrap(),
            bmc_credentials: Credentials::UsernamePassword {
                username: "admin".to_string(),
                password: "bmc_pass".to_string(),
            },
            nvos_credentials: Credentials::UsernamePassword {
                username: "nvadmin".to_string(),
                password: "nvos_pass".to_string(),
            },
            nvos_host_name: None,
        };
        let req = build_registration(&ep);
        assert_eq!(req.vendor, nsm::Vendor::Nvidia as i32);

        let bmc = req.bmc.as_ref().unwrap();
        assert_eq!(bmc.ip_address, "10.0.0.1");
        assert_eq!(bmc.mac_address, "AA:BB:CC:DD:EE:01");
        let bmc_creds = bmc.credentials.as_ref().unwrap();
        assert_eq!(bmc_creds.username, "admin");
        assert_eq!(bmc_creds.password, "bmc_pass");

        let nvos = req.nvos.as_ref().unwrap();
        assert_eq!(nvos.ip_address, "10.0.0.2");
        assert_eq!(nvos.mac_address, "AA:BB:CC:DD:EE:02");
        let nvos_creds = nvos.credentials.as_ref().unwrap();
        assert_eq!(nvos_creds.username, "nvadmin");
        assert_eq!(nvos_creds.password, "nvos_pass");
    }

    #[tokio::test]
    async fn nsm_call_failure_records_the_external_call_histogram() {
        use carbide_instrument::testing::MetricsCapture;

        // A lazy channel to an unroutable endpoint makes the wrapped call
        // itself fail, exercising the RED wrap without a live NSM.
        // A port that was just bound and released: connecting to it is refused
        // deterministically, unlike a fixed low port something might occupy.
        let refused_addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("local addr")
        };
        let channel = Channel::from_shared(format!("http://{refused_addr}"))
            .expect("endpoint")
            .connect_lazy();
        let backend = NsmSwitchBackend {
            client: nsm::nv_switch_manager_client::NvSwitchManagerClient::new(
                TraceInjectService::new(channel),
            ),
        };

        let metrics = MetricsCapture::start();
        backend
            .list_firmware_bundles()
            .await
            .expect_err("unroutable NSM endpoint must fail");

        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "nsm"),
                    ("operation", "list_bundles"),
                    ("outcome", "error"),
                ],
            ),
            1,
        );
    }
}
