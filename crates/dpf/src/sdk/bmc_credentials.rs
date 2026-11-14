/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;

use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use kube::core::ObjectMeta;
use serde_json::json;

use super::{DpfSdk, SECRET_NAME, dpu_device_cr_name};
use crate::crds::dpudevices_generated::DPUDevice;
use crate::error::DpfError;
use crate::repository::{
    BmcCredentialRepository, DpuDeviceRepository, DpuNodeRepository, DpuRepository,
};
use crate::types::DpfDeletePreconditions;

const DEVICE_LABEL: &str = "carbide.nvidia.com/bmc-credential-device";
const VERSION_LABEL: &str = "carbide.nvidia.com/bmc-credential-version";

/// Credential failure reasons reported by DPF's BMCCredentialsReady condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BmcCredentialFailureReason {
    CredentialsSecretNotFound,
    CredentialsSecretInvalid,
    BmcAuthenticationFailed,
    ModeSwitchNotAllowed,
    SharedCredentialSecretNotAllowed,
    /// Preserve unrecognized reasons from newer DPF controllers.
    Other(String),
}

impl From<&str> for BmcCredentialFailureReason {
    fn from(reason: &str) -> Self {
        match reason {
            "CredentialsSecretNotFound" => Self::CredentialsSecretNotFound,
            "CredentialsSecretInvalid" => Self::CredentialsSecretInvalid,
            "BMCAuthenticationFailed" => Self::BmcAuthenticationFailed,
            "ModeSwitchNotAllowed" => Self::ModeSwitchNotAllowed,
            "SharedCredentialSecretNotAllowed" => Self::SharedCredentialSecretNotAllowed,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BmcCredentialHealth {
    /// The condition is absent, unknown, or refers to an older resource generation.
    Pending,
    Ready,
    Failed(BmcCredentialFailureReason),
}

/// One DPUDevice observation, including the identity needed for a conditional reference update.
/// BMC-provided condition messages are omitted because they can contain credential values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BmcCredentialObservation {
    pub device_id: String,
    pub device_uid: String,
    pub resource_version: String,
    pub desired_secret_name: Option<String>,
    pub active_secret_name: Option<String>,
    pub health: BmcCredentialHealth,
}

impl BmcCredentialObservation {
    /// Adoption requires the intended reference in both spec and status plus healthy credentials.
    pub fn is_adopted(&self, secret_name: &str) -> bool {
        self.desired_secret_name.as_deref() == Some(secret_name)
            && self.active_secret_name.as_deref() == Some(secret_name)
            && self.health == BmcCredentialHealth::Ready
    }
}

/// Stable Secret name for a device's confirmed credential version, or its retained initial
/// credential (`None`). No password is encoded.
pub fn bmc_credential_secret_name(device_id: &str, version: Option<u32>) -> String {
    match version {
        Some(version) => format!("{device_id}-v{version}"),
        None => format!("{device_id}-initial"),
    }
}

pub(super) fn validate_per_device_secret_name(secret_name: &str) -> Result<(), DpfError> {
    if secret_name.is_empty() || secret_name == SECRET_NAME {
        return Err(DpfError::InvalidState(
            "a per-device BMC credential Secret name is required".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn check_registration_credential(
    device: &DPUDevice,
    requested: Option<&str>,
) -> Result<(), DpfError> {
    if requested.is_some() && requested != device.spec.bmc_credential_secret_name.as_deref() {
        return Err(DpfError::InvalidState(
            "existing DPUDevice has a different BMC credential reference".to_string(),
        ));
    }
    Ok(())
}

impl<R: BmcCredentialRepository, L> DpfSdk<R, L> {
    fn new_bmc_credential_secret(
        &self,
        device_id: &str,
        version: Option<u32>,
        password: &str,
    ) -> Secret {
        Secret {
            metadata: ObjectMeta {
                name: Some(bmc_credential_secret_name(device_id, version)),
                namespace: Some(self.namespace.clone()),
                labels: Some(BTreeMap::from([
                    (DEVICE_LABEL.to_string(), device_id.to_string()),
                    (
                        VERSION_LABEL.to_string(),
                        version.map_or_else(|| "initial".to_string(), |v| v.to_string()),
                    ),
                ])),
                ..Default::default()
            },
            type_: Some("Opaque".to_string()),
            immutable: Some(true),
            data: Some(BTreeMap::from([(
                "password".to_string(),
                ByteString(password.as_bytes().to_vec()),
            )])),
            ..Default::default()
        }
    }

    /// Create an immutable credential Secret for a new device. An existing Secret is an error,
    /// even when its credential matches; lifecycle cleanup must finish before registration.
    /// `None` identifies an initial credential with no confirmed site-wide version.
    pub async fn create_bmc_credential_secret(
        &self,
        device_id: &str,
        version: Option<u32>,
        password: &str,
    ) -> Result<String, DpfError> {
        if device_id.is_empty() || password.is_empty() {
            return Err(DpfError::InvalidState(
                "DPU identity and BMC password must be nonempty".to_string(),
            ));
        }
        let secret = self.new_bmc_credential_secret(device_id, version, password);
        BmcCredentialRepository::create_bmc_credential_secret(&*self.repo, &secret).await?;
        Ok(bmc_credential_secret_name(device_id, version))
    }

    /// Create an immutable per-device Secret or verify the identical result of an earlier attempt.
    /// `version` is the confirmed credential version; `None` identifies retained initial credentials.
    /// A collision, mutable Secret, or Secret being deleted is an error; nothing is overwritten.
    pub async fn ensure_bmc_credential_secret(
        &self,
        device_id: &str,
        version: Option<u32>,
        password: &str,
    ) -> Result<String, DpfError> {
        if device_id.is_empty() || password.is_empty() {
            return Err(DpfError::InvalidState(
                "DPU identity and BMC password must be nonempty".to_string(),
            ));
        }
        let name = bmc_credential_secret_name(device_id, version);
        let version_label = version.map_or_else(|| "initial".to_string(), |v| v.to_string());
        let existing =
            BmcCredentialRepository::get_bmc_credential_secret(&*self.repo, &name, &self.namespace)
                .await?;
        let secret = match existing {
            Some(secret) => secret,
            None => {
                let desired = self.new_bmc_credential_secret(device_id, version, password);
                match BmcCredentialRepository::create_bmc_credential_secret(&*self.repo, &desired)
                    .await
                {
                    Ok(()) => {}
                    Err(error)
                        if matches!(&error, DpfError::AlreadyExists { .. })
                            || matches!(&error, DpfError::KubeError(kube::Error::Api(status)) if status.is_already_exists()) =>
                        {}
                    Err(error) => return Err(error),
                }
                BmcCredentialRepository::get_bmc_credential_secret(
                    &*self.repo,
                    &name,
                    &self.namespace,
                )
                .await?
                .ok_or_else(|| DpfError::not_found("Secret", &name))?
            }
        };
        let owned = secret
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(DEVICE_LABEL))
            .map(String::as_str)
            == Some(device_id);
        let same_password = secret
            .data
            .as_ref()
            .and_then(|data| data.get("password"))
            .is_some_and(|value| value.0 == password.as_bytes());
        if !owned
            || !same_password
            || secret
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(VERSION_LABEL))
                != Some(&version_label)
            || secret.type_.as_deref() != Some("Opaque")
            || secret.immutable != Some(true)
            || secret.metadata.deletion_timestamp.is_some()
        {
            return Err(DpfError::InvalidState(format!(
                "BMC credential Secret {name} does not match the requested immutable credential"
            )));
        }
        Ok(name)
    }
}

impl<R: BmcCredentialRepository + DpuNodeRepository + DpuDeviceRepository + DpuRepository, L>
    DpfSdk<R, L>
{
    /// Delete all NICo-owned credential revisions for a drained host. Returns `false` while
    /// its DPUNode, DPUDevices, DPUs, or credential Secrets still exist, including finalizer waits.
    /// Secrets are deleted using observed UID/resource-version preconditions; shared and foreign
    /// Secrets are never deleted. On older DPF versions this is a no-op returning `true`.
    /// Callers must serialize cleanup with registration for this host.
    pub async fn delete_host_bmc_credential_secrets(
        &self,
        node_id: &str,
        device_ids: &[String],
    ) -> Result<bool, DpfError> {
        if !self.supports_per_dpu_bmc_credentials() {
            return Ok(true);
        }
        let node_name = super::dpu_node_cr_name(node_id);
        if DpuNodeRepository::get(&*self.repo, &node_name, &self.namespace)
            .await?
            .is_some()
        {
            return Ok(false);
        }
        for id in device_ids {
            if DpuDeviceRepository::get(&*self.repo, &dpu_device_cr_name(id), &self.namespace)
                .await?
                .is_some()
            {
                return Ok(false);
            }
        }
        for id in device_ids {
            if DpuRepository::get(
                &*self.repo,
                &super::dpu_cr_name(id, node_id),
                &self.namespace,
            )
            .await?
            .is_some()
            {
                return Ok(false);
            }
        }
        let mut complete = true;
        for id in device_ids {
            let secrets = BmcCredentialRepository::list_bmc_credential_secrets(
                &*self.repo,
                &self.namespace,
                &format!("{DEVICE_LABEL}={id}"),
            )
            .await?;
            for secret in secrets {
                if secret
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(DEVICE_LABEL))
                    != Some(id)
                {
                    continue;
                }
                let name = required_identity(secret.metadata.name, "credential Secret name")?;
                validate_per_device_secret_name(&name)?;
                let version = name
                    .strip_prefix(&format!("{id}-v"))
                    .and_then(|version| version.parse::<u32>().ok());
                if name != bmc_credential_secret_name(id, version) {
                    continue;
                }
                complete = false;
                if secret.metadata.deletion_timestamp.is_some() {
                    continue;
                }
                let preconditions = DpfDeletePreconditions {
                    uid: required_identity(secret.metadata.uid, "credential Secret UID")?,
                    resource_version: required_identity(
                        secret.metadata.resource_version,
                        "credential Secret resource version",
                    )?,
                };
                BmcCredentialRepository::delete_bmc_credential_secret(
                    &*self.repo,
                    &name,
                    &self.namespace,
                    &preconditions,
                )
                .await?;
            }
        }
        Ok(complete)
    }
}

impl<R: DpuDeviceRepository, L> DpfSdk<R, L> {
    /// Read credential references and health from the same live DPUDevice observation.
    pub async fn observe_bmc_credential(
        &self,
        device_id: &str,
    ) -> Result<BmcCredentialObservation, DpfError> {
        let name = dpu_device_cr_name(device_id);
        let device = DpuDeviceRepository::get(&*self.repo, &name, &self.namespace)
            .await?
            .ok_or_else(|| DpfError::not_found("DPUDevice", &name))?;
        if device.metadata.deletion_timestamp.is_some() {
            return Err(DpfError::InvalidState(format!(
                "DPUDevice {name} is being deleted"
            )));
        }
        let condition = device
            .status
            .as_ref()
            .and_then(|status| status.conditions.as_ref())
            .and_then(|conditions| {
                conditions
                    .iter()
                    .find(|condition| condition.type_ == "BMCCredentialsReady")
            });
        let health = match condition {
            Some(condition)
                if condition
                    .observed_generation
                    .zip(device.metadata.generation)
                    .is_some_and(|(observed, generation)| observed < generation) =>
            {
                BmcCredentialHealth::Pending
            }
            Some(condition) if condition.status == "True" => BmcCredentialHealth::Ready,
            Some(condition) if condition.status == "False" => {
                BmcCredentialHealth::Failed(condition.reason.as_str().into())
            }
            _ => BmcCredentialHealth::Pending,
        };
        Ok(BmcCredentialObservation {
            device_id: device_id.to_string(),
            device_uid: required_identity(device.metadata.uid, "DPUDevice UID")?,
            resource_version: required_identity(
                device.metadata.resource_version,
                "DPUDevice resource version",
            )?,
            desired_secret_name: device.spec.bmc_credential_secret_name,
            active_secret_name: device
                .status
                .and_then(|status| status.bmc_credential_secret_name),
            health,
        })
    }

    /// Patch only the credential reference using the observed UID and resource version.
    /// On conflict, callers must observe again before retrying. This operation does not poll DPF.
    pub async fn set_bmc_credential_reference(
        &self,
        observation: &BmcCredentialObservation,
        secret_name: &str,
    ) -> Result<(), DpfError> {
        validate_per_device_secret_name(secret_name)?;
        if observation.device_uid.is_empty() || observation.resource_version.is_empty() {
            return Err(DpfError::InvalidState(
                "DPUDevice update identity is missing".to_string(),
            ));
        }
        DpuDeviceRepository::patch(
            &*self.repo,
            &dpu_device_cr_name(&observation.device_id),
            &self.namespace,
            json!({
                "metadata": { "uid": observation.device_uid, "resourceVersion": observation.resource_version },
                "spec": { "bmcCredentialSecretName": secret_name },
            }),
        ).await
    }
}

impl<R: BmcCredentialRepository + DpuDeviceRepository, L> DpfSdk<R, L> {
    /// Request deletion of a NICo-owned old Secret only after confirmed adoption of its replacement.
    /// Checks the target DPUDevice's adoption and Secret ownership. Callers must keep each credential
    /// Secret exclusive to its owning DPUDevice and serialize lifecycle work. UID and resource-version
    /// preconditions protect Secret replacements, and DPF's finalizers remain intact. Missing is success.
    pub async fn delete_old_bmc_credential_secret(
        &self,
        device_id: &str,
        old_secret_name: &str,
        adopted_secret_name: &str,
    ) -> Result<(), DpfError> {
        validate_per_device_secret_name(old_secret_name)?;
        validate_per_device_secret_name(adopted_secret_name)?;
        if old_secret_name == adopted_secret_name {
            return Err(DpfError::InvalidState(
                "cannot delete the adopted BMC credential Secret".to_string(),
            ));
        }
        let observation = self.observe_bmc_credential(device_id).await?;
        if !observation.is_adopted(adopted_secret_name) {
            return Err(DpfError::InvalidState(
                "replacement BMC credential adoption is not confirmed".to_string(),
            ));
        }
        let Some(secret) = BmcCredentialRepository::get_bmc_credential_secret(
            &*self.repo,
            old_secret_name,
            &self.namespace,
        )
        .await?
        else {
            return Ok(());
        };
        if secret
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(DEVICE_LABEL))
            .map(String::as_str)
            != Some(device_id)
        {
            return Err(DpfError::InvalidState(
                "BMC credential Secret is not owned by this NICo device".to_string(),
            ));
        }
        let preconditions = DpfDeletePreconditions {
            uid: required_identity(secret.metadata.uid, "secret UID")?,
            resource_version: required_identity(
                secret.metadata.resource_version,
                "secret resource version",
            )?,
        };
        BmcCredentialRepository::delete_bmc_credential_secret(
            &*self.repo,
            old_secret_name,
            &self.namespace,
            &preconditions,
        )
        .await
    }
}

fn required_identity(value: Option<String>, field: &str) -> Result<String, DpfError> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DpfError::InvalidState(format!("{field} is missing")))
}

#[cfg(test)]
mod tests;
