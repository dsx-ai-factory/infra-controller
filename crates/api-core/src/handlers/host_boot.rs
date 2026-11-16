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

use std::net::IpAddr;

use ::rpc::forge as rpc;
use mac_address::MacAddress;
use model::expected_machine::{ExpectedInterfaceIpAllocation, ExpectedMachineRequest};
use model::host_boot::{HostBootIntent, HostBootPhase, HostBootPurpose};
use tonic::{Request, Response};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_request_data_redacted};
use crate::auth::AuthContext;

fn expected_id(value: Option<::rpc::common::Uuid>) -> Result<Uuid, CarbideError> {
    let value = value
        .ok_or_else(|| CarbideError::InvalidArgument("expected_machine_id is required".into()))?;
    Uuid::parse_str(&value.value)
        .map_err(|_| CarbideError::InvalidArgument("expected_machine_id must be a UUID".into()))
}

fn generation(value: &str) -> Result<Uuid, CarbideError> {
    Uuid::parse_str(value)
        .map_err(|_| CarbideError::InvalidArgument("generation must be a UUID".into()))
}

fn purpose(value: i32) -> Result<HostBootPurpose, CarbideError> {
    match rpc::HostBootPurpose::try_from(value) {
        Ok(rpc::HostBootPurpose::Discovery) => Ok(HostBootPurpose::Discovery),
        Ok(rpc::HostBootPurpose::Installation) => Ok(HostBootPurpose::Installation),
        Ok(rpc::HostBootPurpose::Cleanup) => Ok(HostBootPurpose::Cleanup),
        Ok(rpc::HostBootPurpose::LocalDisk) => Ok(HostBootPurpose::LocalDisk),
        _ => Err(CarbideError::InvalidArgument(
            "a supported boot purpose is required".into(),
        )),
    }
}

fn status(intent: HostBootIntent) -> rpc::HostBootStatus {
    let phase = match intent.phase {
        HostBootPhase::Pending => rpc::HostBootPhase::Pending,
        HostBootPhase::Inspecting => rpc::HostBootPhase::Inspecting,
        HostBootPhase::Mounting => rpc::HostBootPhase::Mounting,
        HostBootPhase::Mounted => rpc::HostBootPhase::Mounted,
        HostBootPhase::SettingBoot => rpc::HostBootPhase::SettingBoot,
        HostBootPhase::Armed => rpc::HostBootPhase::Armed,
        HostBootPhase::Resetting => rpc::HostBootPhase::Resetting,
        HostBootPhase::AwaitingAcknowledgement => rpc::HostBootPhase::AwaitingAcknowledgement,
        HostBootPhase::Detaching => rpc::HostBootPhase::Detaching,
        HostBootPhase::Completed => rpc::HostBootPhase::Completed,
        HostBootPhase::Cancelled => rpc::HostBootPhase::Cancelled,
        HostBootPhase::Failed => rpc::HostBootPhase::Failed,
    };
    let purpose = match intent.purpose {
        HostBootPurpose::Discovery => rpc::HostBootPurpose::Discovery,
        HostBootPurpose::Installation => rpc::HostBootPurpose::Installation,
        HostBootPurpose::Cleanup => rpc::HostBootPurpose::Cleanup,
        HostBootPurpose::LocalDisk => rpc::HostBootPurpose::LocalDisk,
    };
    let cleanup_complete = intent.cleanup_complete();
    rpc::HostBootStatus {
        expected_machine_id: Some(::rpc::common::Uuid {
            value: intent.expected_machine_id.to_string(),
        }),
        generation: intent.generation.to_string(),
        purpose: purpose as i32,
        phase: phase as i32,
        artifact_url: intent.artifact_url,
        artifact_sha256: intent.artifact_sha256,
        acknowledgement_token: intent.acknowledgement_token.to_string(),
        acknowledged: intent.acknowledged,
        cleanup_complete,
        last_error: intent.last_error,
        owned_media_uri: intent.media_uri,
        cancelled: intent.cancelled,
        revision: intent.revision,
    }
}

fn validate_artifact(url: &str, digest: &str) -> Result<(), CarbideError> {
    let parsed = url::Url::parse(url).map_err(|_| {
        CarbideError::InvalidArgument("artifact_url must be an absolute HTTP(S) URL".into())
    })?;
    if url.len() > 4096
        || !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(CarbideError::InvalidArgument(
            "artifact_url must be HTTP(S), without credentials or fragment".into(),
        ));
    }
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CarbideError::InvalidArgument(
            "artifact_sha256 must contain 64 lowercase hexadecimal characters".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn set(
    api: &Api,
    request: Request<rpc::SetHostBootIntentRequest>,
) -> Result<Response<rpc::HostBootStatus>, CarbideError> {
    let request = request.into_inner();
    let id = expected_id(request.expected_machine_id)?;
    let generation = generation(&request.generation)?;
    let purpose = purpose(request.purpose)?;
    log_request_data_redacted(format!(
        "expected_machine_id={id}, generation={generation}, purpose={purpose:?}, artifact_sha256={}",
        request.artifact_sha256
    ));
    if request.architecture != rpc::MachineArchitecture::X86 as i32 {
        return Err(CarbideError::InvalidArgument(
            "virtual-media boot requires an explicit X86 architecture declaration".into(),
        ));
    }

    let boot_mac: MacAddress = request.boot_mac_address.parse().map_err(|_| {
        CarbideError::InvalidArgument("boot_mac_address must be a MAC address".into())
    })?;
    let fixed_ip: IpAddr = request.fixed_ip_address.parse().map_err(|_| {
        CarbideError::InvalidArgument("fixed_ip_address must be an IP address".into())
    })?;
    if !fixed_ip.is_ipv4() {
        return Err(CarbideError::InvalidArgument(
            "virtual-media boot supports IPv4 fixed leases".into(),
        ));
    }

    let mut txn = api.txn_begin().await?;
    let machine = db::expected_machine::find_for_update(
        &mut txn,
        &ExpectedMachineRequest {
            id: Some(id),
            bmc_mac_address: None,
        },
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "host_boot_policy",
        id: id.to_string(),
    })?;
    let configured = machine
        .data
        .interfaces
        .iter()
        .find(|interface| interface.role.is_host() && interface.mac_address == Some(boot_mac))
        .ok_or_else(|| {
            CarbideError::FailedPrecondition("boot MAC is not a registered Host interface".into())
        })?;
    if configured.resolved_ip_allocation() != ExpectedInterfaceIpAllocation::Fixed
        || configured.fixed_ip != Some(fixed_ip)
    {
        return Err(CarbideError::FailedPrecondition(
            "external DHCP lease must match the registered Fixed Host MAC/IP".into(),
        ));
    }
    if db::host_boot::resolve_bmc_address(&mut txn, &machine)
        .await?
        .is_none()
    {
        return Err(CarbideError::FailedPrecondition(
            "virtual-media boot requires a known registered BMC address".into(),
        ));
    }
    if machine
        .data
        .interfaces
        .iter()
        .any(|interface| !interface.role.is_host() && !interface.role.is_host_bmc())
        || machine.data.dpf_enabled == Some(true)
    {
        return Err(CarbideError::FailedPrecondition(
            "virtual-media boot does not support DPU or DPF hosts".into(),
        ));
    }

    for interface in
        db::machine_interface::find_by_mac_address(&mut txn, machine.bmc_mac_address).await?
    {
        if let Some(machine_id) = interface.machine_id {
            let known = db::machine::find_one(&mut txn, &machine_id, Default::default()).await?;
            if machine_id.machine_type() == carbide_uuid::machine::MachineType::Dpu
                || known.is_some_and(|host| !host.associated_dpu_machine_ids().is_empty())
            {
                return Err(CarbideError::FailedPrecondition(
                    "virtual-media boot does not support a managed host with attached DPUs".into(),
                ));
            }
        }
    }

    let previous = db::host_boot::find(&mut txn, id).await?;
    let (artifact_url, artifact_sha256) =
        if purpose == HostBootPurpose::LocalDisk && request.artifact_url.is_empty() {
            let previous = previous.as_ref().ok_or_else(|| {
                CarbideError::FailedPrecondition(
                    "local disk handoff requires an existing boot policy".into(),
                )
            })?;
            (
                previous.artifact_url.clone(),
                previous.artifact_sha256.clone(),
            )
        } else {
            validate_artifact(&request.artifact_url, &request.artifact_sha256)?;
            (request.artifact_url, request.artifact_sha256)
        };
    if let Some(previous) = &previous {
        if previous.generation == generation {
            if previous.requested_purpose != purpose
                || previous.artifact_url != artifact_url
                || previous.artifact_sha256 != artifact_sha256
                || previous.boot_mac_address != boot_mac
                || previous.fixed_ip_address != fixed_ip
                || previous.bmc_mac_address != machine.bmc_mac_address
            {
                return Err(CarbideError::AlreadyInProgress(
                    "generation already exists with different immutable boot parameters".into(),
                ));
            }
            let response = status(previous.clone());
            txn.commit().await?;
            return Ok(Response::new(response));
        }
        if !previous.phase.is_terminal()
            || previous.media_uri.is_some()
            || previous.previous_boot.is_some()
        {
            return Err(CarbideError::FailedPrecondition(
                "previous boot generation must finish owned-media cleanup before replacement"
                    .into(),
            ));
        }
    }
    let mut intent = HostBootIntent {
        expected_machine_id: id,
        bmc_mac_address: machine.bmc_mac_address,
        boot_mac_address: boot_mac,
        fixed_ip_address: fixed_ip,
        generation,
        purpose,
        requested_purpose: purpose,
        artifact_url,
        artifact_sha256,
        phase: HostBootPhase::Pending,
        media_uri: None,
        previous_boot: None,
        acknowledgement_token: Uuid::new_v4(),
        acknowledged: false,
        cancelled: false,
        reset_attempted_at: None,
        phase_started_at: None,
        last_error: None,
        failed_from_phase: None,
        lifecycle_key: None,
        revision: previous.as_ref().map_or(0, |row| row.revision),
    };
    if previous.is_some() {
        if !db::host_boot::update(&mut txn, &intent).await? {
            return Err(CarbideError::FailedPrecondition(
                "boot progress changed; read it before retrying".into(),
            ));
        }
        intent.revision += 1;
    } else {
        db::host_boot::create(&mut txn, &intent).await?;
    }
    txn.commit().await?;
    Ok(Response::new(status(intent)))
}

pub(crate) async fn get(
    api: &Api,
    request: Request<rpc::GetHostBootIntentRequest>,
) -> Result<Response<rpc::HostBootStatus>, CarbideError> {
    let id = expected_id(request.into_inner().expected_machine_id)?;
    log_request_data_redacted(format!("expected_machine_id={id}"));
    let row = db::host_boot::find(&api.database_connection, id).await?;
    Ok(Response::new(row.map(status).unwrap_or_else(|| {
        rpc::HostBootStatus {
            expected_machine_id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            ..Default::default()
        }
    })))
}

pub(crate) async fn acknowledge(
    api: &Api,
    request: Request<rpc::AcknowledgeHostBootRequest>,
) -> Result<Response<rpc::HostBootStatus>, CarbideError> {
    let machine_identity = crate::auth::authenticated_machine_id(&request)?;
    let privileged = request
        .extensions()
        .get::<AuthContext>()
        .is_some_and(|auth| {
            auth.principals.iter().any(|principal| {
                matches!(
                    principal,
                    carbide_authn::middleware::Principal::ExternalUser(_)
                )
            }) || auth.get_spiffe_service_id() == Some("elektra-site-agent")
        });
    let request = request.into_inner();
    let id = expected_id(request.expected_machine_id)?;
    let generation = generation(&request.generation)?;
    log_request_data_redacted(format!(
        "expected_machine_id={id}, generation={generation}, evidence={}",
        request.evidence
    ));
    let token = Uuid::parse_str(&request.acknowledgement_token).map_err(|_| {
        CarbideError::InvalidArgument("acknowledgement_token must be a UUID".into())
    })?;
    let evidence = rpc::HostBootEvidence::try_from(request.evidence)
        .map_err(|_| CarbideError::InvalidArgument("unknown boot evidence".into()))?;
    let mut txn = api.txn_begin().await?;
    let mut row =
        db::host_boot::find(&mut txn, id)
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "host_boot_policy",
                id: id.to_string(),
            })?;
    if row.generation != generation || row.acknowledgement_token != token {
        return Err(CarbideError::FailedPrecondition(
            "boot evidence does not match the current generation and attempt".into(),
        ));
    }
    if !row.accepts_acknowledgement(generation, token) {
        return Err(CarbideError::FailedPrecondition(
            "host is not awaiting current boot evidence".into(),
        ));
    }
    match evidence {
        rpc::HostBootEvidence::ScoutRegistered => {
            let machine_id = request.machine_id.ok_or_else(|| {
                CarbideError::InvalidArgument("Scout evidence requires machine_id".into())
            })?;
            if machine_identity != Some(machine_id) {
                return Err(CarbideError::PermissionDeniedError(
                    "Scout evidence requires its authenticated machine identity".into(),
                ));
            }
            let interfaces =
                db::machine_interface::find_by_mac_address(&mut txn, row.boot_mac_address).await?;
            if !interfaces
                .iter()
                .any(|interface| interface.machine_id == Some(machine_id))
            {
                return Err(CarbideError::PermissionDeniedError(
                    "Scout identity does not own the registered boot interface".into(),
                ));
            }
        }
        rpc::HostBootEvidence::InstallerIndependent if privileged => {}
        _ => {
            return Err(CarbideError::PermissionDeniedError(
                "installer evidence requires a privileged orchestrator".into(),
            ));
        }
    }
    if !row.acknowledged {
        row.acknowledged = true;
        if !db::host_boot::update(&mut txn, &row).await? {
            return Err(CarbideError::FailedPrecondition(
                "boot attempt changed; stale evidence was not accepted".into(),
            ));
        }
        row.revision += 1;
    }
    txn.commit().await?;
    Ok(Response::new(status(row)))
}

pub(crate) async fn cancel(
    api: &Api,
    request: Request<rpc::CancelHostBootRequest>,
) -> Result<Response<rpc::HostBootStatus>, CarbideError> {
    let request = request.into_inner();
    let id = expected_id(request.expected_machine_id)?;
    let generation = generation(&request.generation)?;
    log_request_data_redacted(format!("expected_machine_id={id}, generation={generation}"));
    let work_lock = api
        .work_lock_manager_handle
        .try_acquire_lock(format!("host-virtual-media:{id}"))
        .await
        .map_err(|_| {
            CarbideError::UnavailableError(
                "host boot operation is in flight; retry cancellation after it is observed".into(),
            )
        })?;
    let mut txn = api.txn_begin().await?;
    work_lock.fence_transaction(&mut txn).await?;
    let mut row =
        db::host_boot::find(&mut txn, id)
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "host_boot_policy",
                id: id.to_string(),
            })?;
    if row.generation != generation {
        return Err(CarbideError::FailedPrecondition(
            "cancellation belongs to a different generation".into(),
        ));
    }
    if !row.cancelled {
        row.cancelled = true;
        // Preserve uncertain dispatch boundaries. The worker must first observe
        // any reserved write before it can prove owned-media cleanup.
        if !matches!(
            row.phase,
            HostBootPhase::Failed
                | HostBootPhase::Mounting
                | HostBootPhase::SettingBoot
                | HostBootPhase::Resetting
                | HostBootPhase::AwaitingAcknowledgement
        ) {
            row.phase = HostBootPhase::Detaching;
            row.phase_started_at = None;
        }
        row.last_error = None;
        if !db::host_boot::update(&mut txn, &row).await? {
            return Err(CarbideError::FailedPrecondition(
                "boot progress changed; read it before retrying cancellation".into(),
            ));
        }
        row.revision += 1;
    }
    txn.commit().await?;
    Ok(Response::new(status(row)))
}

pub(crate) async fn release(
    api: &Api,
    request: Request<rpc::CancelHostBootRequest>,
) -> Result<Response<()>, CarbideError> {
    let request = request.into_inner();
    let id = expected_id(request.expected_machine_id)?;
    let generation = generation(&request.generation)?;
    log_request_data_redacted(format!("expected_machine_id={id}, generation={generation}"));
    let mut txn = api.txn_begin().await?;
    let row =
        db::host_boot::find(&mut txn, id)
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "host_boot_policy",
                id: id.to_string(),
            })?;
    if row.generation != generation {
        return Err(CarbideError::FailedPrecondition(
            "policy release belongs to a different generation".into(),
        ));
    }
    if !row.cleanup_complete() {
        return Err(CarbideError::FailedPrecondition(
            "owned-media and boot-state cleanup must complete before returning to native PXE"
                .into(),
        ));
    }
    if !db::host_boot::delete_cleaned(&mut txn, &row).await? {
        return Err(CarbideError::FailedPrecondition(
            "boot progress changed; read it before releasing policy ownership".into(),
        ));
    }
    txn.commit().await?;
    Ok(Response::new(()))
}
