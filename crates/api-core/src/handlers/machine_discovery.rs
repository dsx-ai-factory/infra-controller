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

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub use ::rpc::forge as rpc;
use carbide_uuid::machine::MachineIdSource;
use carbide_uuid::nvlink::NvLinkDomainId;
use db::WithTransaction;
use futures_util::FutureExt;
use model::hardware_info::{GpuPlatformInfo, HardwareInfo, MachineNvLinkInfo, NvLinkGpu};
use model::machine::machine_id::{from_hardware_info, host_id_from_dpu_hardware_info};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{DpuInitState, DpuInitStates, ManagedHostState};
use tonic::{Request, Response, Status};

use crate::api::{Api, log_machine_id, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;
use crate::{CarbideError, attestation as attest};

pub(crate) async fn discover_machine(
    api: &Api,
    request: Request<rpc::MachineDiscoveryInfo>,
) -> Result<Response<rpc::MachineDiscoveryResult>, Status> {
    // We don't log_request_data(&request); here because the hardware info is huge
    let remote_ip: Option<IpAddr> = match request.metadata().get("X-Forwarded-For") {
        None => {
            // Normal production case.
            // This is set in api/src/listener.rs::listen_and_serve when we `accept` the connection
            // The IP is usually an IPv4-mapped IPv6 addresses (e.g. `::ffff:10.217.133.10`) so
            // we use to_canonical() to convert it to IPv4.
            request
                .extensions()
                .get::<Arc<carbide_authn::middleware::ConnectionAttributes>>()
                .map(|conn_attrs| conn_attrs.peer_address.ip().to_canonical())
        }
        Some(ip_str) => {
            // Development case, we override the remote IP with HTTP header
            ip_str
                .to_str()
                .ok()
                .and_then(|s| s.parse().map(|ip: IpAddr| ip.to_canonical()).ok())
        }
    };

    let machine_discovery_info = request.into_inner();
    let interface_id = machine_discovery_info.machine_interface_id;
    let discovery_reporter = machine_discovery_info.discovery_reporter();

    let discovery_data = machine_discovery_info
        .discovery_data
        .map(|data| match data {
            rpc::machine_discovery_info::DiscoveryData::Info(info) => info,
        })
        .ok_or_else(|| {
            CarbideError::InvalidArgument("Discovery data is not populated".to_string())
        })?;
    let attest_key_info_opt = discovery_data.attest_key_info.clone();
    let hardware_info = HardwareInfo::try_from(discovery_data).map_err(CarbideError::from)?;

    // this is an early check for certificate creation that happens later on in this method.
    // let's save us the hassle and return immediately if the below condition is not satisfied
    if api.runtime_config.attestation_enabled
        && !hardware_info.is_dpu()
        && attest_key_info_opt.is_none()
    {
        return Err(
            CarbideError::InvalidArgument("AttestKeyInfo is not populated".to_string()).into(),
        );
    }

    // Generate a stable Machine ID based on the hardware information
    let stable_machine_id = from_hardware_info(&hardware_info).map_err(|e| {
            CarbideError::InvalidArgument(
                format!("Insufficient HardwareInfo to derive a Stable Machine ID for Machine on InterfaceId {interface_id:?}: {e}"),
            )
        })?;
    log_machine_id(&stable_machine_id);

    // Build NVLink info from scout GPU platform info. domain_uuid is backfilled by the
    // NVLink partition monitor from NMX-C hello.
    let gpu_platform_infos: Vec<&GpuPlatformInfo> = hardware_info
        .gpus
        .iter()
        .filter_map(|gpu| gpu.platform_info.as_ref())
        .collect();

    let nvlink_info = if hardware_info.is_mnnvl_capable()
        && !gpu_platform_infos.is_empty()
        && api
            .runtime_config
            .nvlink_config
            .as_ref()
            .is_some_and(|config| config.enabled)
    {
        nvlink_info_from_gpu_platform_infos(&gpu_platform_infos)
    } else {
        None
    };

    // Admission permit BEFORE the transaction: waiters on the admin-segment
    // advisory lock must queue in memory, not on open pool connections.
    let _admin_admission = db::machine_interface::admin_lock_admission().await;
    let mut txn = api.txn_begin().await?;
    tracing::debug!(
        ?remote_ip,
        ?interface_id,
        "discover_machine loading interface"
    );

    if !hardware_info.is_dpu()
        && hardware_info.tpm_ek_certificate.is_none()
        && api.runtime_config.tpm_required
    {
        return Err(CarbideError::InvalidArgument(format!(
                "Ignoring DiscoverMachine request for non-tpm enabled host with InterfaceId {interface_id:?}"
            ))
            .into());
    } else if !hardware_info.is_dpu() && hardware_info.tpm_ek_certificate.is_some() {
        // this means we do have an EK cert for a host

        // get the EK cert from incoming message
        let tpm_ek_cert =
            hardware_info
                .tpm_ek_certificate
                .as_ref()
                .ok_or(CarbideError::InvalidArgument(
                    "tpm_ek_cert is empty".to_string(),
                ))?;

        attest::match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            tpm_ek_cert,
            &stable_machine_id,
        )
        .await?;
    }

    let interface =
        db::machine_interface::find_by_ip_or_id(&mut txn, remote_ip, interface_id).await?;
    if !hardware_info.is_dpu()
        && hardware_info.tpm_ek_certificate.is_none()
        && stable_machine_id.source() == MachineIdSource::ProductBoardChassisSerial
        && let Some(existing_machine_id) = interface.machine_id
        && existing_machine_id.source() == MachineIdSource::Tpm
        && existing_machine_id.machine_type().is_host()
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "TPM EK certificate missing for host discovery on InterfaceId {interface_id:?}; refusing to derive serial-based machine id {stable_machine_id} for existing TPM-derived machine id {existing_machine_id}"
        ))
        .into());
    }
    let machine_id = if hardware_info.is_dpu() {
        // if site explorer is creating machine records and there isn't one for this machine return an error
        if api
            .runtime_config
            .site_explorer
            .create_machines
            .load(Ordering::Relaxed)
        {
            db::machine::find_one(
                &mut txn,
                &stable_machine_id,
                MachineSearchConfig {
                    include_dpus: true,
                    ..MachineSearchConfig::default()
                },
            )
            .await?
            .ok_or_else(|| {
                CarbideError::InvalidArgument(format!(
                    "Machine id {stable_machine_id} was not discovered by site-explorer."
                ))
            })?;
        }

        let db_machine = if machine_discovery_info.create_machine {
            let machine = db::machine::get_or_create(
                &mut txn,
                Some(&api.common_pools),
                &stable_machine_id,
                &interface,
            )
            .await?;

            // Update the record only when create_machine is enabled.
            // Site-explorer will update if machine is created by site-explorer.
            db::machine_interface::associate_interface_with_dpu_machine(
                &interface.id,
                &stable_machine_id,
                &mut txn,
            )
            .await?;
            machine
        } else {
            db::machine::find_one(
                &mut txn,
                &stable_machine_id,
                MachineSearchConfig {
                    include_dpus: true,
                    ..MachineSearchConfig::default()
                },
            )
            .await?
            .ok_or_else(|| {
                CarbideError::InvalidArgument(format!("Machine id {stable_machine_id} not found."))
            })?
        };

        if db_machine.network_config.loopback_ip.is_none() {
            let loopback_ip = db::machine::allocate_loopback_ip(
                &api.common_pools,
                &mut txn,
                &stable_machine_id.to_string(),
            )
            .await?;

            let mut network_config = db_machine.network_config.value.clone();
            network_config.loopback_ip = Some(loopback_ip);
            db::machine::try_update_network_config(
                &mut txn,
                &stable_machine_id,
                db_machine.network_config.version,
                &network_config,
            )
            .await?;
        }

        if api
            .runtime_config
            .vmaas_config
            .as_ref()
            .map(|vc| vc.secondary_overlay_support)
            .unwrap_or_default()
            && db_machine
                .network_config
                .secondary_overlay_vtep_ip
                .is_none()
        {
            let secondary_vtep_ip = db::machine::allocate_secondary_vtep_ip(
                &api.common_pools,
                &mut txn,
                &stable_machine_id.to_string(),
            )
            .await?;

            let mut network_config = db_machine.network_config.value.clone();
            network_config.secondary_overlay_vtep_ip = Some(secondary_vtep_ip);
            db::machine::try_update_network_config(
                &mut txn,
                &stable_machine_id,
                db_machine.network_config.version,
                &network_config,
            )
            .await?;
        }

        db_machine.id
    } else {
        // Now we know stable machine id for host. Let's update it in db.
        db::machine::try_sync_stable_id_with_current_machine_id_for_host(
            &mut txn,
            &interface.machine_id,
            &stable_machine_id,
        )
        .await?
    };

    db::machine_topology::create_or_update_with_bom_validation(
        &mut txn,
        &stable_machine_id,
        &hardware_info,
        api.runtime_config.bom_validation.enabled,
    )
    .await?;

    if hardware_info.is_dpu() {
        // Create Host proactively.
        // In case host interface is created, this method will return existing one, instead
        // creating new everytime.
        let machine_interface =
            db::machine_interface::create_host_machine_dpu_interface_proactively(
                &mut txn,
                Some(&hardware_info),
                &machine_id,
                api.runtime_config.retained_boot_interface_window,
            )
            .await?;

        let host_machine_id = if let Some(host_machine_id) = machine_interface.machine_id {
            host_machine_id
        } else {
            // Create host machine with temporary ID if no machine is attached.
            let predicted_machine_id =
                host_id_from_dpu_hardware_info(&hardware_info).map_err(|e| {
                    CarbideError::InvalidArgument(format!("hardware info missing: {e}"))
                })?;

            let host_has_primary = db::machine_interface::find_by_machine_ids(
                &mut txn,
                std::slice::from_ref(&predicted_machine_id),
            )
            .await?
            .get(&predicted_machine_id)
            .is_some_and(|interfaces| {
                interfaces
                    .iter()
                    .any(|interface| interface.primary_interface)
            });
            if host_has_primary && machine_interface.primary_interface {
                db::machine_interface::set_primary_interface(
                    &machine_interface.id,
                    false,
                    &mut txn,
                )
                .await?;
            }

            let mi_id = machine_interface.id;
            let proactive_machine = db::machine::get_or_create(
                &mut txn,
                Some(&api.common_pools),
                &predicted_machine_id,
                &machine_interface,
            )
            .await?;

            // Update host and DPUs state correctly.
            db::machine::update_state(
                &mut txn,
                &proactive_machine.id,
                &ManagedHostState::DPUInit {
                    dpu_states: DpuInitStates {
                        states: HashMap::from([(machine_id, DpuInitState::Init)]),
                    },
                },
            )
            .await?;

            tracing::info!(
                ?mi_id,
                machine_id = %proactive_machine.id,
                "Created host machine proactively",
            );

            proactive_machine.id
        };

        // Normalize admin address ownership any time DPU discovery creates
        // or reattaches a DPU-backed host interface.
        let active_config_changed =
            db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &host_machine_id)
                .await?;
        if active_config_changed {
            let (network_config, network_config_version) =
                db::machine::get_network_config(&mut txn, &host_machine_id)
                    .await?
                    .take();
            db::machine::try_update_network_config(
                &mut txn,
                &host_machine_id,
                network_config_version,
                &network_config,
            )
            .await?;
        }
    }

    // if attestation is enabled and it is not a DPU, then we create a random nonce (auth token)
    // and create a decrypting challenge (make credential) out of it.
    // Whoever was able to decrypt it (activate credential), possesses
    // the TPM that the endorsement key (EK) and the attestation key (AK) that they came from.
    // if attestation is not enabled, or it is a DPU, then issue machine certificates immediately
    let attest_key_challenge = if api.runtime_config.attestation_enabled && !hardware_info.is_dpu()
    {
        let Some(attest_key_info) = attest_key_info_opt else {
            return Err(CarbideError::InvalidArgument(
                "Internal Error: This should have been handled above! AttestKeyInfo is not populated.".into(),
            )
            .into());
        };

        tracing::info!(
            "It is not a DPU and attestation is enabled. Generating Attest Key Bind Challenge ..."
        );
        Some(
            crate::handlers::measured_boot::create_attest_key_bind_challenge(
                &mut txn,
                &attest_key_info,
                &stable_machine_id,
            )
            .await?,
        )
    } else {
        tracing::info!(
            "Attestation enabled is {}. Is_DPU is {}. Vending certs to machine with id {}",
            api.runtime_config.attestation_enabled,
            hardware_info.is_dpu(),
            stable_machine_id,
        );

        None
    };

    if let Some(nvlink_info) = nvlink_info {
        db::machine::update_nvlink_info(&mut txn, &machine_id, nvlink_info).await?;
    }

    if discovery_reporter == rpc::MachineDiscoveryReporter::Scout
        && let Some(scout_version) = machine_discovery_info
            .discovery_reporter_version
            .as_deref()
            .filter(|v| !v.is_empty())
    {
        db::machine::update_last_scout_observed_version(
            &stable_machine_id,
            scout_version,
            &mut txn,
        )
        .await?;
    }

    txn.commit().await?;

    let machine_certificate = if attest_key_challenge.is_none() {
        if std::env::var("UNSUPPORTED_CERTIFICATE_PROVIDER").is_ok() {
            Some(rpc::MachineCertificate::default())
        } else {
            Some(
                api.certificate_provider
                    .get_certificate(&stable_machine_id.to_string(), None, None)
                    .await
                    .map_err(|err| CarbideError::ClientCertificateError(err.to_string()))?
                    .into(),
            )
        }
    } else {
        None
    };

    let response = Ok(Response::new(rpc::MachineDiscoveryResult {
        machine_id: Some(stable_machine_id),
        machine_certificate,
        attest_key_challenge,
        machine_interface_id: Some(interface.id),
    }));

    if hardware_info.is_dpu()
        && let Some(dpu_info) = hardware_info.dpu_info.as_ref()
    {
        // WARNING: DONOT REUSE OLD TXN HERE. IT WILL CREATE DEADLOCK.
        //
        // Create a new transaction here for network devices. Inner transaction is not so
        // helpful in postgres and using same transaction creates deadlock with
        // machine_interface table.

        // Create DPU and LLDP Association.
        api.with_txn(|txn| {
            db::network_devices::dpu_to_network_device_map::create_dpu_network_device_association(
                txn,
                &dpu_info.switches,
                &stable_machine_id,
            )
            .boxed()
        })
        .await??;
    }

    response
}

// Host has completed discovery
pub(crate) async fn discovery_completed(
    api: &Api,
    request: Request<rpc::MachineDiscoveryCompletedRequest>,
) -> Result<Response<rpc::MachineDiscoveryCompletedResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let machine_id = convert_and_log_machine_id(req.machine_id.as_ref())?;

    let (machine, mut txn) = api
        .load_machine(&machine_id, MachineSearchConfig::default())
        .await?;
    db::machine::update_discovery_time(&machine.id, &mut txn).await?;

    let discovery_result = "Success".to_owned();

    txn.commit().await?;

    tracing::info!(
        %machine_id,
        discovery_result, "discovery_completed",
    );
    Ok(Response::new(rpc::MachineDiscoveryCompletedResponse {}))
}

/// Builds NVLink discovery info from scout `GpuPlatformInfo` for every GPU that reported it.
fn nvlink_info_from_gpu_platform_infos(
    platform_infos: &[&GpuPlatformInfo],
) -> Option<MachineNvLinkInfo> {
    let chassis_serial = platform_infos
        .first()
        .map(|p| p.chassis_serial.clone())
        .unwrap_or_default();
    if chassis_serial.trim().is_empty() {
        return None;
    }

    let gpus = platform_infos
        .iter()
        .map(|platform_info| {
            let guid = {
                let s = platform_info.fabric_guid.trim();
                if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    u64::from_str_radix(hex, 16).unwrap_or(0)
                } else {
                    s.parse::<u64>().unwrap_or(0)
                }
            };
            NvLinkGpu {
                tray_index: platform_info.tray_index as i32,
                slot_id: platform_info.slot_number as i32,
                device_id: platform_info.module_id as i32,
                guid,
            }
        })
        .collect();

    Some(MachineNvLinkInfo {
        domain_uuid: NvLinkDomainId::nil(),
        chassis_serial,
        gpus,
    })
}
