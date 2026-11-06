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

//! Checks that only read: every read operation of every capability.

use std::time::{Duration, Instant};

use bmc_platform::{Capability, ConsoleSpec, EvidenceProgress, PlatformError};

use super::context::CheckCx;
use super::errors::failed;
use super::model::{Check, Checked, Detail, Needs, Outcome, Step};
use super::text::{boot_order_summary, counted, difference_detail, reported, yes_no};

/// How long a signed-measurement request may stay pending.
const EVIDENCE_TIMEOUT: Duration = Duration::from_secs(180);
const EVIDENCE_POLL_INTERVAL: Duration = Duration::from_secs(5);

pub(super) fn checks() -> Vec<Check> {
    vec![
        Check::read("power.state", Capability::Power, run!(power_state)),
        Check::read(
            "power.ac_power_cycle_supported",
            Capability::Power,
            run!(ac_power_cycle_supported),
        ),
        Check::read(
            "bmc_control.ipmi_over_lan",
            Capability::BmcControl,
            run!(ipmi_over_lan),
        ),
        Check::read(
            "bmc_control.settings_status",
            Capability::BmcControl,
            run!(manager_settings_status),
        ),
        Check::read("bios.status", Capability::Bios, run!(bios_status)),
        Check::read("bios.infinite_boot", Capability::Bios, run!(infinite_boot)),
        Check::read(
            "boot_order.status",
            Capability::BootOrder,
            run!(boot_order_status),
        )
        .needs(&[Needs::BootInterface]),
        Check::read(
            "secure_boot.status",
            Capability::SecureBoot,
            run!(secure_boot_status),
        ),
        Check::read(
            "secure_boot.platform_key",
            Capability::SecureBoot,
            run!(platform_key),
        ),
        Check::read(
            "lockdown.status",
            Capability::Lockdown,
            run!(lockdown_status),
        ),
        Check::read("accounts.list", Capability::Accounts, run!(accounts_list)),
        Check::read(
            "firmware.inventory",
            Capability::Firmware,
            run!(firmware_inventory),
        ),
        Check::read(
            "storage.boot_controller",
            Capability::Storage,
            run!(boot_controller),
        ),
        Check::read("dpu.status", Capability::Dpu, run!(dpu_status)),
        Check::read(
            "attestation.components",
            Capability::Attestation,
            run!(attestation_components),
        ),
        Check::read(
            "attestation.evidence",
            Capability::Attestation,
            run!(attestation_evidence),
        )
        .timeout(EVIDENCE_TIMEOUT + Duration::from_secs(60)),
        Check::read("console.status", Capability::Console, run!(console_status)),
        Check::read("console.spec", Capability::Console, run!(console_spec)),
        Check::read(
            "super_nic.east_west",
            Capability::SuperNic,
            run!(super_nic_east_west),
        )
        .needs(&[Needs::SuperNics]),
    ]
}

async fn power_state(cx: &CheckCx<'_>) -> Step {
    let state = cx.bmc().power()?.state().await?;
    Ok(match state {
        Some(state) => format!("{state:?}"),
        None => "the platform reports no single power state".to_string(),
    })
}

async fn ac_power_cycle_supported(cx: &CheckCx<'_>) -> Step {
    let supported = cx.bmc().power()?.ac_power_cycle_supported().await?;
    Ok(yes_no(supported, "supported", "not supported"))
}

async fn ipmi_over_lan(cx: &CheckCx<'_>) -> Step {
    let enabled = cx.bmc().bmc_control()?.ipmi_over_lan_enabled().await?;
    Ok(yes_no(enabled, "enabled", "disabled"))
}

async fn manager_settings_status(cx: &CheckCx<'_>) -> Checked {
    let status = cx.bmc().bmc_control()?.settings_status().await?;
    Ok(difference_detail(status.differences.iter().map(|diff| {
        (diff.key.as_str(), &diff.expected, diff.actual.as_ref())
    })))
}

async fn bios_status(cx: &CheckCx<'_>) -> Checked {
    let status = cx
        .bmc()
        .bios()?
        .status(
            &cx.inputs().bios_profile,
            cx.inputs().boot_interface().as_ref(),
        )
        .await?;
    Ok(difference_detail(status.differences.iter().map(|diff| {
        (diff.key.as_str(), &diff.expected, diff.actual.as_ref())
    })))
}

async fn infinite_boot(cx: &CheckCx<'_>) -> Step {
    let enabled = cx.bmc().bios()?.infinite_boot_enabled().await?;
    Ok(match enabled {
        Some(enabled) => yes_no(enabled, "enabled", "disabled"),
        None => "the platform has no infinite-boot setting".to_string(),
    })
}

async fn boot_order_status(cx: &CheckCx<'_>) -> Step {
    let selector = cx.inputs().required_boot_interface()?;
    let status = cx.bmc().boot_order()?.status(&selector).await?;
    Ok(boot_order_summary(status))
}

async fn secure_boot_status(cx: &CheckCx<'_>) -> Step {
    let status = cx.bmc().secure_boot()?.status().await?;
    Ok(format!("{status:?}"))
}

async fn platform_key(cx: &CheckCx<'_>) -> Step {
    let present = cx.bmc().secure_boot()?.has_platform_key().await?;
    Ok(yes_no(present, "platform key enrolled", "no platform key"))
}

/// The aggregate with its host and BMC parts, then the controls the driver read.
async fn lockdown_status(cx: &CheckCx<'_>) -> Checked {
    let status = cx.bmc().lockdown()?.status().await?;
    Ok(with_message_fields(
        format!(
            "{:?} · host {:?} · bmc {:?}",
            status.aggregate, status.host, status.bmc
        ),
        &status.message,
    ))
}

/// The account this run logged in with must be listed.
async fn accounts_list(cx: &CheckCx<'_>) -> Step {
    let accounts = cx.bmc().accounts()?.list().await?;
    let names: Vec<&str> = accounts
        .iter()
        .filter_map(|account| account.user_name.as_deref())
        .filter(|name| !name.is_empty())
        .collect();
    if !names.contains(&cx.target().username()) {
        return Err(Outcome::Fail(format!(
            "the account this run logged in with is not listed; listed: {}",
            names.join(", ")
        )));
    }
    Ok(format!(
        "{}: {}",
        counted(names.len(), "account", "accounts"),
        names.join(", ")
    ))
}

async fn firmware_inventory(cx: &CheckCx<'_>) -> Checked {
    let inventory = cx.bmc().firmware()?.inventory().await?;
    Ok(Detail {
        summary: counted(inventory.len(), "entry", "entries"),
        fields: inventory
            .iter()
            .map(|entry| (entry.id.clone(), reported(&entry.version.clone().flatten())))
            .collect(),
    })
}

async fn boot_controller(cx: &CheckCx<'_>) -> Step {
    let controller = cx.bmc().storage()?.boot_controller().await?;
    Ok(match controller {
        Some(id) => format!("boot controller {id}"),
        None => "no boot controller".to_string(),
    })
}

async fn dpu_status(cx: &CheckCx<'_>) -> Step {
    let status = cx.bmc().dpu()?.status().await?;
    Ok(format!(
        "nic mode {} · host rshim {}",
        reported(&status.nic_mode),
        reported(&status.host_rshim)
    ))
}

async fn attestation_components(cx: &CheckCx<'_>) -> Step {
    let components = cx.bmc().attestation()?.components().await?;
    let ids: Vec<&str> = components
        .iter()
        .map(|component| component.id.as_str())
        .collect();
    Ok(if ids.is_empty() {
        "none listed".to_string()
    } else {
        format!(
            "{}: {}",
            counted(ids.len(), "component", "components"),
            ids.join(", ")
        )
    })
}

/// Collects signed measurements from the first component, with its
/// certificate and firmware.
async fn attestation_evidence(cx: &CheckCx<'_>) -> Checked {
    let bmc = cx.bmc();
    let attestation = bmc.attestation()?;
    let components = attestation
        .components()
        .await
        .map_err(failed("components"))?;
    let Some(component) = components.first() else {
        return Err(Outcome::Skipped(
            "the BMC lists no ComponentIntegrity resources".to_string(),
        ));
    };
    let id = component.id.as_str();
    let certificate = attestation
        .ca_certificate(id)
        .await
        .map_err(failed("ca_certificate"))?;
    let firmware = match attestation.firmware_for_component(id).await {
        Ok(firmware) => firmware
            .version
            .clone()
            .flatten()
            .unwrap_or_else(|| firmware.id.clone()),
        Err(PlatformError::Unsupported) => "not linked on this platform".to_string(),
        Err(error) => return Err(failed("firmware_for_component")(error)),
    };

    let nonce: [u8; 32] = rand::random();
    let mut progress = attestation
        .request_evidence(id, &nonce)
        .await
        .map_err(failed("request_evidence"))?;
    let deadline = Instant::now() + EVIDENCE_TIMEOUT;
    loop {
        match progress {
            EvidenceProgress::Ready(_) => {
                return Ok(Detail {
                    summary: format!("signed measurements from {id}"),
                    fields: vec![
                        ("certificate".to_string(), certificate.id.clone()),
                        ("firmware".to_string(), firmware),
                    ],
                });
            }
            EvidenceProgress::Failed { state, messages } => {
                let messages: Vec<&str> = messages
                    .iter()
                    .filter_map(|message| message.message.as_deref())
                    .collect();
                return Err(Outcome::Fail(format!(
                    "measurement collection for {id} ended {state:?}: {}",
                    messages.join("; ")
                )));
            }
            EvidenceProgress::Pending(reference) => {
                if Instant::now() + EVIDENCE_POLL_INTERVAL > deadline {
                    return Err(Outcome::Fail(format!(
                        "measurement collection {} still pending after {}s",
                        reference.uri(),
                        EVIDENCE_TIMEOUT.as_secs()
                    )));
                }
                tokio::time::sleep(EVIDENCE_POLL_INTERVAL).await;
                progress = attestation
                    .poll_evidence(&reference)
                    .await
                    .map_err(failed("poll_evidence"))?;
            }
        }
    }
}

async fn console_status(cx: &CheckCx<'_>) -> Checked {
    let status = cx.bmc().console()?.status().await?;
    Ok(with_message_fields(
        format!("{:?}", status.state),
        &status.message,
    ))
}

async fn console_spec(cx: &CheckCx<'_>) -> Step {
    let spec = cx.bmc().console()?.spec().await?;
    Ok(match spec {
        ConsoleSpec::SshShell(shell) => format!("SSH shell on port {}", shell.port),
        ConsoleSpec::SshDirect { port } => format!("direct SSH on port {port}"),
        ConsoleSpec::IpmiSol { port, .. } => format!("IPMI SOL on port {port}"),
        ConsoleSpec::None { reason } => format!("no console transport: {reason}"),
    })
}

/// Whether each SuperNIC's embedded CPU manages its east-west traffic.
async fn super_nic_east_west(cx: &CheckCx<'_>) -> Checked {
    let count = cx.inputs().required_super_nics()?;
    let bmc = cx.bmc();
    let super_nic = bmc.super_nic()?;
    let mut fields = Vec::with_capacity(count.into());
    for index in 0..count {
        let state = super_nic
            .east_west_control(index)
            .await
            .map_err(failed("east_west_control"))?;
        let state = match state {
            Some(enabled) => yes_no(enabled, "enabled", "disabled"),
            None => "unreported".to_string(),
        };
        fields.push((format!("SuperNIC {index}"), state));
    }
    Ok(Detail {
        summary: format!(
            "east-west control on {}",
            counted(count.into(), "SuperNIC", "SuperNICs")
        ),
        fields,
    })
}

/// `summary` with a driver message's `key=value, key=value` pairs as fields;
/// a message in any other shape is appended to the summary.
fn with_message_fields(summary: String, message: &str) -> Detail {
    let pairs: Option<Vec<(String, String)>> = message
        .split(", ")
        .map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (!key.is_empty() && !key.contains(char::is_whitespace))
                .then(|| (key.to_string(), value.to_string()))
        })
        .collect();
    match pairs {
        _ if message.is_empty() => summary.into(),
        Some(fields) => Detail { summary, fields },
        None => format!("{summary} ({message})").into(),
    }
}
