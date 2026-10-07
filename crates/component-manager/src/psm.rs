// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_instrument::red;
use carbide_secrets::credentials::Credentials;
use model::component_manager::{FirmwareState, PowerAction, PowerShelfComponent};
use tonic::transport::Channel;
use trace_propagation::TraceInjectService;
use tracing::instrument;

use crate::config::BackendTlsConfig;
use crate::error::ComponentManagerError;
use crate::power_shelf_manager::{
    PowerShelfComponentResult, PowerShelfEndpoint, PowerShelfFirmwareUpdateStatus,
    PowerShelfFirmwareVersions, PowerShelfManager, PowerShelfPowerStateResult, PowerShelfVendor,
};
use crate::proto::psm;
use crate::types::parse_mac;

#[derive(Debug)]
pub struct PsmPowerShelfBackend {
    client: psm::powershelf_manager_client::PowershelfManagerClient<TraceInjectService<Channel>>,
}

impl PsmPowerShelfBackend {
    pub async fn connect(
        url: &str,
        tls: Option<&BackendTlsConfig>,
    ) -> Result<Self, ComponentManagerError> {
        let channel = crate::tls::build_channel(url, tls, "PSM").await?;
        Ok(Self {
            client: psm::powershelf_manager_client::PowershelfManagerClient::new(channel),
        })
    }
}

fn map_psm_fw_state(state: i32) -> FirmwareState {
    match psm::FirmwareUpdateState::try_from(state) {
        Ok(psm::FirmwareUpdateState::Queued) => FirmwareState::Queued,
        Ok(psm::FirmwareUpdateState::Verifying) => FirmwareState::Verifying,
        Ok(psm::FirmwareUpdateState::Completed) => FirmwareState::Completed,
        Ok(psm::FirmwareUpdateState::Failed) => FirmwareState::Failed,
        _ => FirmwareState::Unknown,
    }
}

fn map_vendor(v: &PowerShelfVendor) -> i32 {
    match v {
        PowerShelfVendor::Unknown => psm::PmcVendor::PmcTypeUnknown as i32,
        PowerShelfVendor::Liteon => psm::PmcVendor::PmcTypeLiteon as i32,
    }
}

fn credentials_to_psm(creds: &Credentials) -> psm::Credentials {
    match creds {
        Credentials::UsernamePassword { username, password } => psm::Credentials {
            username: username.clone(),
            password: password.clone(),
        },
    }
}

fn to_psm_component(c: &PowerShelfComponent) -> psm::PowershelfComponent {
    match c {
        PowerShelfComponent::Pmc => psm::PowershelfComponent::Pmc,
        PowerShelfComponent::Psu => psm::PowershelfComponent::Psu,
    }
}

fn mac_strings(endpoints: &[PowerShelfEndpoint]) -> Vec<String> {
    endpoints.iter().map(|ep| ep.pmc_mac.to_string()).collect()
}

/// Registers endpoints with PSM. PSM uses PMC MAC as its identifier, so
/// registration is primarily about ensuring PSM knows about the device and
/// has credentials.
async fn register_with_psm(
    client: &mut psm::powershelf_manager_client::PowershelfManagerClient<
        TraceInjectService<Channel>,
    >,
    endpoints: &[PowerShelfEndpoint],
) -> Result<(), ComponentManagerError> {
    let reqs: Vec<psm::RegisterPowershelfRequest> = endpoints
        .iter()
        .map(|ep| psm::RegisterPowershelfRequest {
            pmc_mac_address: ep.pmc_mac.to_string(),
            pmc_ip_address: ep.pmc_ip.to_string(),
            pmc_vendor: map_vendor(&ep.pmc_vendor),
            pmc_credentials: Some(credentials_to_psm(&ep.pmc_credentials)),
        })
        .collect();

    let response = red::instrumented(
        "psm",
        "register_powershelves",
        client.register_powershelves(psm::RegisterPowershelvesRequest {
            registration_requests: reqs,
        }),
    )
    .await?
    .into_inner();

    let failures: Vec<_> = response
        .responses
        .iter()
        .filter(|r| r.status != psm::StatusCode::Success as i32)
        .collect();

    if failures.len() == endpoints.len() && !endpoints.is_empty() {
        let errors: Vec<String> = failures.iter().map(|f| f.error.clone()).collect();
        return Err(ComponentManagerError::Internal(format!(
            "PSM registration failed for all power shelves: {}",
            errors.join("; ")
        )));
    }

    for f in &failures {
        tracing::warn!(
            pmc_mac_address = %f.pmc_mac_address,
            error = %f.error,
            "PSM registration failed for power shelf"
        );
    }

    Ok(())
}

#[async_trait::async_trait]
impl PowerShelfManager for PsmPowerShelfBackend {
    fn name(&self) -> &str {
        "psm"
    }

    #[instrument(skip(self), fields(backend = "psm"))]
    async fn power_control(
        &self,
        endpoints: &[PowerShelfEndpoint],
        action: PowerAction,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        register_with_psm(&mut self.client.clone(), endpoints).await?;

        let pmc_macs = mac_strings(endpoints);
        let request = psm::PowershelfRequest {
            pmc_macs: pmc_macs.clone(),
        };

        let response = match action {
            PowerAction::On => {
                red::instrumented("psm", "power_on", self.client.clone().power_on(request))
                    .await?
                    .into_inner()
            }
            PowerAction::ForceOff | PowerAction::GracefulShutdown => {
                red::instrumented("psm", "power_off", self.client.clone().power_off(request))
                    .await?
                    .into_inner()
            }
            PowerAction::GracefulRestart
            | PowerAction::ForceRestart
            | PowerAction::AcPowercycle => {
                let off = red::instrumented(
                    "psm",
                    "power_off",
                    self.client.clone().power_off(psm::PowershelfRequest {
                        pmc_macs: pmc_macs.clone(),
                    }),
                )
                .await?
                .into_inner();

                let mut results: Vec<PowerShelfComponentResult> = Vec::new();
                let mut powered_off_macs: Vec<String> = Vec::new();

                for r in off.responses {
                    if r.status == psm::StatusCode::Success as i32 {
                        powered_off_macs.push(r.pmc_mac_address);
                    } else {
                        results.push(PowerShelfComponentResult {
                            pmc_mac: parse_mac(&r.pmc_mac_address)?,
                            success: false,
                            error: if r.error.is_empty() {
                                None
                            } else {
                                Some(r.error)
                            },
                        });
                    }
                }

                if !powered_off_macs.is_empty() {
                    let on = red::instrumented(
                        "psm",
                        "power_on",
                        self.client.clone().power_on(psm::PowershelfRequest {
                            pmc_macs: powered_off_macs,
                        }),
                    )
                    .await?
                    .into_inner();

                    for r in on.responses {
                        results.push(PowerShelfComponentResult {
                            pmc_mac: parse_mac(&r.pmc_mac_address)?,
                            success: r.status == psm::StatusCode::Success as i32,
                            error: if r.error.is_empty() {
                                None
                            } else {
                                Some(r.error)
                            },
                        });
                    }
                }

                return Ok(results);
            }
        };

        response
            .responses
            .into_iter()
            .map(|r| {
                Ok(PowerShelfComponentResult {
                    pmc_mac: parse_mac(&r.pmc_mac_address)?,
                    success: r.status == psm::StatusCode::Success as i32,
                    error: if r.error.is_empty() {
                        None
                    } else {
                        Some(r.error)
                    },
                })
            })
            .collect()
    }

    #[instrument(skip(self), fields(backend = "psm"))]
    async fn update_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
        target_version: &str,
        components: &[PowerShelfComponent],
        _options: &crate::types::FirmwareUpdateOptions,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        register_with_psm(&mut self.client.clone(), endpoints).await?;

        let psm_components: Vec<i32> = components
            .iter()
            .map(|c| to_psm_component(c) as i32)
            .collect();

        let upgrades: Vec<psm::UpdatePowershelfFirmwareRequest> = endpoints
            .iter()
            .map(|ep| {
                let component_reqs = psm_components
                    .iter()
                    .map(|&comp| psm::UpdateComponentFirmwareRequest {
                        component: comp,
                        upgrade_to: Some(psm::FirmwareVersion {
                            version: target_version.to_owned(),
                        }),
                    })
                    .collect();
                psm::UpdatePowershelfFirmwareRequest {
                    pmc_mac_address: ep.pmc_mac.to_string(),
                    components: component_reqs,
                }
            })
            .collect();

        let request = psm::UpdateFirmwareRequest { upgrades };

        let response = red::instrumented(
            "psm",
            "update_firmware",
            self.client.clone().update_firmware(request),
        )
        .await?
        .into_inner();

        response
            .responses
            .into_iter()
            .map(|r| {
                let any_error = r
                    .components
                    .iter()
                    .any(|c| c.status != psm::StatusCode::Success as i32);
                let error_msg = r
                    .components
                    .iter()
                    .filter(|c| !c.error.is_empty())
                    .map(|c| c.error.clone())
                    .collect::<Vec<_>>()
                    .join("; ");
                Ok(PowerShelfComponentResult {
                    pmc_mac: parse_mac(&r.pmc_mac_address)?,
                    success: !any_error,
                    error: if error_msg.is_empty() {
                        None
                    } else {
                        Some(error_msg)
                    },
                })
            })
            .collect()
    }

    #[instrument(skip(self), fields(backend = "psm"))]
    async fn get_firmware_status(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareUpdateStatus>, ComponentManagerError> {
        register_with_psm(&mut self.client.clone(), endpoints).await?;

        let queries = endpoints
            .iter()
            .flat_map(|ep| {
                let mac = ep.pmc_mac.to_string();
                vec![
                    psm::FirmwareUpdateQuery {
                        pmc_mac_address: mac,
                        component: psm::PowershelfComponent::Pmc as i32,
                    },
                    /*
                    TODO: support retrieving fw status gracefully
                    psm::FirmwareUpdateQuery {
                        pmc_mac_address: mac,
                        component: psm::PowershelfComponent::Psu as i32,
                    },
                    */
                ]
            })
            .collect();

        let request = psm::GetFirmwareUpdateStatusRequest { queries };

        let response = red::instrumented(
            "psm",
            "get_firmware_update_status",
            self.client.clone().get_firmware_update_status(request),
        )
        .await?
        .into_inner();

        response
            .statuses
            .into_iter()
            .map(|s| {
                Ok(PowerShelfFirmwareUpdateStatus {
                    pmc_mac: parse_mac(&s.pmc_mac_address)?,
                    state: map_psm_fw_state(s.state),
                    target_version: String::new(),
                    error: if s.error.is_empty() {
                        None
                    } else {
                        Some(s.error)
                    },
                })
            })
            .collect()
    }

    #[instrument(skip(self), fields(backend = "psm"))]
    async fn list_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareVersions>, ComponentManagerError> {
        register_with_psm(&mut self.client.clone(), endpoints).await?;

        let request = psm::PowershelfRequest {
            pmc_macs: mac_strings(endpoints),
        };

        let response = red::instrumented(
            "psm",
            "list_available_firmware",
            self.client.clone().list_available_firmware(request),
        )
        .await?
        .into_inner();

        response
            .upgrades
            .into_iter()
            .map(|af| {
                let pmc_mac = parse_mac(&af.pmc_mac_address)?;
                let versions = af
                    .upgrades
                    .into_iter()
                    .flat_map(|cu| cu.upgrades.into_iter().map(|fv| fv.version))
                    .collect();
                Ok(PowerShelfFirmwareVersions {
                    pmc_mac,
                    versions,
                    error: None,
                })
            })
            .collect()
    }

    #[instrument(skip(self), fields(backend = "psm"))]
    async fn get_power_state(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfPowerStateResult>, ComponentManagerError> {
        register_with_psm(&mut self.client.clone(), endpoints).await?;

        let request = psm::PowershelfRequest {
            pmc_macs: mac_strings(endpoints),
        };

        let response = red::instrumented(
            "psm",
            "get_powershelves",
            self.client.clone().get_powershelves(request),
        )
        .await?
        .into_inner();

        let mut results = Vec::with_capacity(endpoints.len());
        for ep in endpoints {
            let Some(shelf) = response.powershelves.iter().find(|s| {
                s.pmc
                    .as_ref()
                    .is_some_and(|pmc| pmc.mac_address == ep.pmc_mac.to_string())
            }) else {
                results.push(PowerShelfPowerStateResult {
                    pmc_mac: ep.pmc_mac,
                    power_state: None,
                    error: Some("power shelf not found in PSM inventory".into()),
                });
                continue;
            };

            let powered_on = shelf.psus.iter().any(|psu| psu.power_state);
            results.push(PowerShelfPowerStateResult {
                pmc_mac: ep.pmc_mac,
                power_state: Some(if powered_on { "on" } else { "off" }.into()),
                error: None,
            });
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn psm_fw_state_maps_each_variant() {
        value_scenarios!(run = |state: psm::FirmwareUpdateState| map_psm_fw_state(state as i32);
            "states" {
                psm::FirmwareUpdateState::Queued => FirmwareState::Queued,
                psm::FirmwareUpdateState::Verifying => FirmwareState::Verifying,
                psm::FirmwareUpdateState::Completed => FirmwareState::Completed,
                psm::FirmwareUpdateState::Failed => FirmwareState::Failed,
            }
        );
    }

    #[test]
    fn psm_fw_state_unknown_for_unrecognized_value() {
        value_scenarios!(map_psm_fw_state:
            "unrecognized" {
                9999 => FirmwareState::Unknown,
            }
        );
    }

    #[test]
    fn vendor_maps_each_variant() {
        value_scenarios!(run = |vendor: PowerShelfVendor| map_vendor(&vendor);
            "vendors" {
                PowerShelfVendor::Unknown => psm::PmcVendor::PmcTypeUnknown as i32,
                PowerShelfVendor::Liteon => psm::PmcVendor::PmcTypeLiteon as i32,
            }
        );
    }

    #[test]
    fn mac_strings_from_endpoints() {
        let eps = vec![
            PowerShelfEndpoint {
                pmc_ip: "10.0.0.1".parse().unwrap(),
                pmc_mac: "AA:BB:CC:DD:EE:01".parse().unwrap(),
                pmc_vendor: PowerShelfVendor::Liteon,
                pmc_credentials: Credentials::UsernamePassword {
                    username: "admin".into(),
                    password: "pass".into(),
                },
            },
            PowerShelfEndpoint {
                pmc_ip: "10.0.0.2".parse().unwrap(),
                pmc_mac: "AA:BB:CC:DD:EE:02".parse().unwrap(),
                pmc_vendor: PowerShelfVendor::Unknown,
                pmc_credentials: Credentials::UsernamePassword {
                    username: "admin".into(),
                    password: "pass".into(),
                },
            },
        ];
        let macs = mac_strings(&eps);
        assert_eq!(macs, vec!["AA:BB:CC:DD:EE:01", "AA:BB:CC:DD:EE:02"]);
    }

    #[tokio::test]
    async fn psm_call_failure_records_the_external_call_histogram() {
        use carbide_instrument::testing::MetricsCapture;

        // A lazy channel to an unroutable endpoint makes the wrapped call
        // itself fail, exercising the RED wrap without a live PSM.
        // A port that was just bound and released: connecting to it is refused
        // deterministically, unlike a fixed low port something might occupy.
        let refused_addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("local addr")
        };
        let channel = Channel::from_shared(format!("http://{refused_addr}"))
            .expect("endpoint")
            .connect_lazy();
        let backend = PsmPowerShelfBackend {
            client: psm::powershelf_manager_client::PowershelfManagerClient::new(
                TraceInjectService::new(channel),
            ),
        };

        let metrics = MetricsCapture::start();
        backend
            .list_firmware(&[])
            .await
            .expect_err("unroutable PSM endpoint must fail");

        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "psm"),
                    ("operation", "register_powershelves"),
                    ("outcome", "error"),
                ],
            ),
            1,
        );
    }
}
