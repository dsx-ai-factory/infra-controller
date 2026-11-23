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
    BMC_CREDENTIAL_DEVICE_LABEL, BmcCredentialRepository, DpuDeviceRepository,
};
use crate::types::DpfDeletePreconditions;

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

/// Stable Secret name for one device and publication generation. No password is encoded.
/// The generation is independent of site-wide credential versions.
pub fn bmc_credential_secret_name(device_id: &str, revision: u32) -> String {
    format!("{device_id}-v{revision}")
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
    /// Publish the supplied password using the latest matching device Secret or a new generation.
    /// Only the highest owned generation is reused; older matching passwords receive a new name.
    /// Terminating or malformed Secrets reserve their generations. The retained latest Secret
    /// makes allocation restart-safe without a separate counter. Generation exhaustion is an error.
    ///
    /// Callers must serialize this device's credential lifecycle and supply its stored password
    /// after hardware convergence. A conflicting allocation returns [`DpfError::AlreadyExists`];
    /// reread the authoritative password before retrying instead of advancing a stale publication.
    pub async fn ensure_current_bmc_credential_secret(
        &self,
        device_id: &str,
        password: &str,
    ) -> Result<String, DpfError> {
        if device_id.is_empty() || password.is_empty() {
            return Err(DpfError::InvalidState(
                "DPU identity and BMC password must be nonempty".to_string(),
            ));
        }
        let latest = BmcCredentialRepository::list_bmc_credential_secrets(
            &*self.repo,
            device_id,
            &self.namespace,
        )
        .await?
        .into_iter()
        .filter(|secret| owns_bmc_credential_secret(secret, device_id))
        .filter_map(|secret| {
            let revision = bmc_credential_revision(device_id, secret.metadata.name.as_deref()?)?;
            Some((revision, secret))
        })
        .max_by_key(|(revision, _)| *revision);
        let revision = match latest {
            Some((revision, secret))
                if bmc_credential_secret_matches(&secret, device_id, password) =>
            {
                revision
            }
            Some((revision, _)) => revision.checked_add(1).ok_or_else(|| {
                DpfError::InvalidState(
                    "BMC credential publication generation exhausted".to_string(),
                )
            })?,
            None => 0,
        };
        match self
            .ensure_bmc_credential_secret(device_id, revision, password)
            .await
        {
            // The selected generation changed after listing. Never overwrite it or silently
            // allocate another generation using a password that may already be stale.
            Err(DpfError::InvalidState(_)) => Err(DpfError::already_exists(
                "Secret",
                bmc_credential_secret_name(device_id, revision),
            )),
            result => result,
        }
    }

    /// Create an immutable per-device Secret or verify the identical result of an earlier attempt.
    /// A collision, mutable Secret, or Secret being deleted is an error; nothing is overwritten.
    pub async fn ensure_bmc_credential_secret(
        &self,
        device_id: &str,
        revision: u32,
        password: &str,
    ) -> Result<String, DpfError> {
        if device_id.is_empty() || password.is_empty() {
            return Err(DpfError::InvalidState(
                "DPU identity and BMC password must be nonempty".to_string(),
            ));
        }
        let name = bmc_credential_secret_name(device_id, revision);
        let existing =
            BmcCredentialRepository::get_bmc_credential_secret(&*self.repo, &name, &self.namespace)
                .await?;
        let secret = match existing {
            Some(secret) => secret,
            None => {
                let desired = Secret {
                    metadata: ObjectMeta {
                        name: Some(name.clone()),
                        namespace: Some(self.namespace.clone()),
                        labels: Some(BTreeMap::from([(
                            BMC_CREDENTIAL_DEVICE_LABEL.to_string(),
                            device_id.to_string(),
                        )])),
                        ..Default::default()
                    },
                    type_: Some("Opaque".to_string()),
                    immutable: Some(true),
                    data: Some(BTreeMap::from([(
                        "password".to_string(),
                        ByteString(password.as_bytes().to_vec()),
                    )])),
                    ..Default::default()
                };
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
        if !bmc_credential_secret_matches(&secret, device_id, password) {
            return Err(DpfError::InvalidState(format!(
                "BMC credential Secret {name} does not match the requested immutable credential"
            )));
        }
        Ok(name)
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
    /// Delete owned credential Secrets whose publication generations precede the adopted one.
    /// The adopted name must be generated by [`bmc_credential_secret_name`] for this device.
    /// Each deletion rechecks adoption and uses the protections of
    /// [`Self::delete_old_bmc_credential_secret`]. Newer pending generations and unrelated
    /// Secrets are retained. Keeping the adopted Secret preserves the publication counter across
    /// restarts. Listing on every call makes partial cleanup safe to retry.
    pub async fn delete_obsolete_bmc_credential_secrets(
        &self,
        device_id: &str,
        adopted_secret_name: &str,
    ) -> Result<(), DpfError> {
        let adopted_revision =
            bmc_credential_revision(device_id, adopted_secret_name).ok_or_else(|| {
                DpfError::InvalidState(
                    "adopted BMC credential Secret name does not identify a publication generation"
                        .to_string(),
                )
            })?;
        let secrets = BmcCredentialRepository::list_bmc_credential_secrets(
            &*self.repo,
            device_id,
            &self.namespace,
        )
        .await?;
        for secret in secrets {
            let Some(name) = secret.metadata.name.as_deref() else {
                continue;
            };
            if owns_bmc_credential_secret(&secret, device_id)
                && bmc_credential_revision(device_id, name)
                    .is_some_and(|revision| revision < adopted_revision)
            {
                self.delete_old_bmc_credential_secret(device_id, name, adopted_secret_name)
                    .await?;
            }
        }
        Ok(())
    }

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
        if !owns_bmc_credential_secret(&secret, device_id) {
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

fn bmc_credential_revision(device_id: &str, secret_name: &str) -> Option<u32> {
    let revision = secret_name
        .strip_prefix(&format!("{device_id}-v"))?
        .parse()
        .ok()?;
    (bmc_credential_secret_name(device_id, revision) == secret_name).then_some(revision)
}

fn owns_bmc_credential_secret(secret: &Secret, device_id: &str) -> bool {
    secret
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get(BMC_CREDENTIAL_DEVICE_LABEL))
        .is_some_and(|owner| owner == device_id)
}

fn bmc_credential_secret_matches(secret: &Secret, device_id: &str, password: &str) -> bool {
    owns_bmc_credential_secret(secret, device_id)
        && secret
            .data
            .as_ref()
            .and_then(|data| data.get("password"))
            .is_some_and(|value| value.0 == password.as_bytes())
        && secret.type_.as_deref() == Some("Opaque")
        && secret.immutable == Some(true)
        && secret.metadata.deletion_timestamp.is_none()
}

fn required_identity(value: Option<String>, field: &str) -> Result<String, DpfError> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DpfError::InvalidState(format!("{field} is missing")))
}

#[cfg(test)]
mod tests;
