/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Observe and operate virtual media through the existing authenticated BMC client.
//!
//! The caller owns persistence and authorization. These operations never select an
//! occupied slot or eject a medium without matching the frozen artifact URL.

use std::collections::BTreeSet;
use std::time::Duration;

use carbide_secrets::credentials::Credentials;
use carbide_utils::redfish::BmcAccessInfo;
use libredfish::{Redfish, RedfishError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// One optical virtual-media slot and the actions advertised by its BMC.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirtualMediaSlot {
    /// BMC-relative, absolute Redfish resource URI.
    pub uri: String,
    /// URI of the mounted image, when present.
    pub image: Option<String>,
    /// Whether the BMC reports inserted media.
    pub inserted: bool,
    /// Advertised optical-media support.
    pub optical: bool,
    /// Standard InsertMedia action target, when advertised.
    pub insert_target: Option<String>,
    /// Standard EjectMedia action target, when advertised.
    pub eject_target: Option<String>,
    /// Lenovo XCC1 EXT slots use PATCH rather than standard action targets.
    pub lenovo_ext: bool,
}

impl VirtualMediaSlot {
    /// Whether this slot contains the exact artifact owned by an intent.
    pub fn contains(&self, artifact_url: &str) -> bool {
        self.inserted && self.image.as_deref() == Some(artifact_url)
    }

    /// Whether this slot is empty and supports an insertion operation.
    pub fn available(&self) -> bool {
        !self.inserted
            && self.image.as_ref().is_none_or(String::is_empty)
            && self.optical
            && ((self.insert_target.is_some() && self.eject_target.is_some()) || self.lenovo_ext)
    }
}

/// Current boot override and virtual media observed from one BMC.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirtualMediaObservation {
    /// ComputerSystem resource on which boot overrides are applied.
    pub system_uri: String,
    /// Current Boot object, retained for owned override restoration.
    pub boot: Value,
    /// Current power state from the same observation.
    pub power_state: Option<String>,
    /// Serial reported by the selected ComputerSystem for identity confirmation.
    pub serial_number: Option<String>,
    /// Processor architecture when explicitly reported by the BMC.
    pub architecture: Option<String>,
    /// Advertised boot targets. An absent list is not proof of CD support.
    pub boot_targets: Vec<String>,
    /// Discovered slots in stable URI order.
    pub slots: Vec<VirtualMediaSlot>,
}

impl VirtualMediaObservation {
    /// Require explicit target and UEFI capability before attaching any media.
    pub fn supports_uefi_once(&self, target: &str) -> bool {
        let uefi_allowed = self
            .boot
            .get("BootSourceOverrideMode@Redfish.AllowableValues")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("UEFI")));
        let uefi_selected = self
            .boot
            .get("BootSourceOverrideMode")
            .and_then(Value::as_str)
            == Some("UEFI");
        self.boot_targets.iter().any(|allowed| allowed == target)
            && allowed_when_present(
                &self.boot,
                "BootSourceOverrideEnabled@Redfish.AllowableValues",
                "Once",
            )
            && (uefi_allowed || uefi_selected)
            && allowed_when_present(
                &self.boot,
                "BootSourceOverrideMode@Redfish.AllowableValues",
                "UEFI",
            )
    }

    /// Whether a one-shot optical boot is visibly armed.
    pub fn optical_boot_armed(&self) -> bool {
        self.one_shot_boot_armed("Cd")
    }

    /// Whether a one-shot UEFI boot to a specific target is visibly armed.
    pub fn one_shot_boot_armed(&self, target: &str) -> bool {
        self.boot
            .get("BootSourceOverrideTarget")
            .and_then(Value::as_str)
            == Some(target)
            && self
                .boot
                .get("BootSourceOverrideEnabled")
                .and_then(Value::as_str)
                == Some("Once")
            && self
                .boot
                .get("BootSourceOverrideMode")
                .and_then(Value::as_str)
                == Some("UEFI")
    }
}

/// Redfish operations scoped to an existing authenticated client.
pub struct VirtualMediaClient<'a> {
    client: &'a dyn Redfish,
}

impl<'a> VirtualMediaClient<'a> {
    /// Reuse Core's existing direct or proxy-authenticated BMC client.
    pub fn new(client: &'a dyn Redfish) -> Self {
        Self { client }
    }

    /// Read boot capabilities and manager/system virtual-media collections.
    pub async fn observe(&self) -> Result<VirtualMediaObservation, RedfishError> {
        let standard = self.client.std_redfish();
        let system_uri = format!("/redfish/v1/Systems/{}", standard.system_id());
        let system = self.get(&system_uri).await?;
        let boot = system.get("Boot").cloned().unwrap_or_else(|| json!({}));
        let boot_targets = boot
            .get("BootSourceOverrideTarget@Redfish.AllowableValues")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut collections = BTreeSet::new();
        if let Some(uri) = link(&system, "VirtualMedia") {
            collections.insert(uri.to_owned());
        }
        for manager_id in self.client.get_managers().await? {
            let manager = self
                .get(&format!("/redfish/v1/Managers/{manager_id}"))
                .await?;
            if let Some(uri) = link(&manager, "VirtualMedia") {
                collections.insert(uri.to_owned());
            }
        }
        let lenovo = matches!(
            standard.vendor,
            Some(libredfish::model::service_root::RedfishVendor::Lenovo)
        );
        let mut slots = Vec::new();
        for collection in collections {
            let collection = self.get(&collection).await?;
            let members = collection
                .get("Members")
                .and_then(Value::as_array)
                .ok_or_else(|| unsupported("virtual-media collection has no Members"))?;
            if members.len() > 32 {
                return Err(unsupported("virtual-media collection exceeds 32 slots"));
            }
            for member in members {
                let uri = member
                    .get("@odata.id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| unsupported("virtual-media member has no local resource URI"))?;
                let resource = self.get(uri).await?;
                let types = resource.get("MediaTypes").and_then(Value::as_array);
                let optical = types.is_some_and(|types| {
                    types
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|kind| matches!(kind, "CD" | "DVD"))
                });
                let id = resource
                    .get("Id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let lenovo_ext = lenovo
                    && id.strip_prefix("EXT").is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                    });
                let actions = resource.get("Actions");
                let action = |name| {
                    actions
                        .and_then(|actions| actions.get(name))
                        .and_then(|action| action.get("target"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                };
                slots.push(VirtualMediaSlot {
                    uri: uri.to_owned(),
                    image: resource
                        .get("Image")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned),
                    inserted: resource
                        .get("Inserted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    optical: optical || lenovo_ext,
                    insert_target: action("#VirtualMedia.InsertMedia"),
                    eject_target: action("#VirtualMedia.EjectMedia"),
                    lenovo_ext,
                });
            }
        }
        slots.sort_by(|left, right| left.uri.cmp(&right.uri));
        slots.dedup_by(|left, right| left.uri == right.uri);
        Ok(VirtualMediaObservation {
            system_uri,
            boot,
            power_state: system
                .get("PowerState")
                .and_then(Value::as_str)
                .map(str::to_owned),
            serial_number: system
                .get("SerialNumber")
                .and_then(Value::as_str)
                .map(str::to_owned),
            architecture: system
                .get("ProcessorSummary")
                .and_then(|summary| summary.get("Architecture"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            boot_targets,
            slots,
        })
    }

    /// Insert only into an observed empty optical slot; callers must read back.
    pub async fn insert(
        &self,
        slot: &VirtualMediaSlot,
        artifact_url: &str,
    ) -> Result<(), RedfishError> {
        if !slot.available() {
            return Err(unsupported(
                "refusing to replace occupied or unsupported virtual media",
            ));
        }
        if let Some(target) = &slot.insert_target {
            self.client
                .std_redfish()
                .client
                .post(
                    local_path(target)?,
                    json!({
                        "Image": artifact_url, "Inserted": true, "WriteProtected": true
                    }),
                )
                .await?;
        } else if slot.lenovo_ext {
            self.client
                .std_redfish()
                .client
                .patch(
                    local_path(&slot.uri)?,
                    json!({
                        "Image": artifact_url, "Inserted": true, "WriteProtected": true
                    }),
                )
                .await?;
        } else {
            return Err(unsupported("virtual-media insertion is not supported"));
        }
        Ok(())
    }

    /// Eject only media whose observed image equals the intent's frozen URL.
    pub async fn eject(
        &self,
        slot: &VirtualMediaSlot,
        artifact_url: &str,
    ) -> Result<(), RedfishError> {
        if !slot.contains(artifact_url) {
            return Err(unsupported(
                "refusing to eject media not owned by this intent",
            ));
        }
        if let Some(target) = &slot.eject_target {
            self.client
                .std_redfish()
                .client
                .post(local_path(target)?, json!({}))
                .await?;
        } else if slot.lenovo_ext {
            self.client
                .std_redfish()
                .client
                .patch(
                    local_path(&slot.uri)?,
                    json!({
                        "Image": "", "Inserted": false
                    }),
                )
                .await?;
        } else {
            return Err(unsupported("virtual-media ejection is not supported"));
        }
        Ok(())
    }

    /// Set an advertised one-shot boot target; completion requires readback.
    pub async fn arm(
        &self,
        observation: &VirtualMediaObservation,
        target: &str,
    ) -> Result<(), RedfishError> {
        if !observation.supports_uefi_once(target) {
            return Err(unsupported(
                "this BMC does not prove support for the requested one-shot UEFI boot target",
            ));
        }
        self.client.std_redfish().client.patch(local_path(&observation.system_uri)?, json!({
            "Boot": {"BootSourceOverrideTarget": target, "BootSourceOverrideEnabled": "Once", "BootSourceOverrideMode": "UEFI"}
        })).await?;
        Ok(())
    }

    /// Restore only the boot-override fields Core previously changed.
    pub async fn restore_boot(
        &self,
        observation: &VirtualMediaObservation,
        previous: &Value,
        owned_target: &str,
    ) -> Result<(), RedfishError> {
        // A different active override belongs to another actor and must not be overwritten.
        if !observation.one_shot_boot_armed(owned_target) {
            return Ok(());
        }
        let enabled = previous
            .get("BootSourceOverrideEnabled")
            .and_then(Value::as_str)
            .unwrap_or("Disabled");
        let target = previous
            .get("BootSourceOverrideTarget")
            .and_then(Value::as_str)
            .unwrap_or("None");
        let mut boot =
            json!({"BootSourceOverrideTarget": target, "BootSourceOverrideEnabled": enabled});
        if let Some(mode) = previous
            .get("BootSourceOverrideMode")
            .and_then(Value::as_str)
        {
            boot["BootSourceOverrideMode"] = Value::String(mode.to_owned());
        }
        self.client
            .std_redfish()
            .client
            .patch(local_path(&observation.system_uri)?, json!({"Boot": boot}))
            .await?;
        Ok(())
    }

    async fn get(&self, uri: &str) -> Result<Value, RedfishError> {
        self.client
            .std_redfish()
            .client
            .get(local_path(uri)?)
            .await
            .map(|(_, value)| value)
    }
}

/// Verify a small immutable bootstrap artifact before issuing BMC mutations.
///
/// This request uses ordinary server TLS verification and never follows a
/// redirect. The 64 MiB limit prevents an accidental operating-system ISO from
/// becoming an unbounded controller download.
pub async fn verify_artifact(
    artifact_url: &str,
    expected_sha256: &str,
) -> Result<(), RedfishError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(transport_error)?;
    let mut response = client
        .get(artifact_url)
        .send()
        .await
        .map_err(transport_error)?
        .error_for_status()
        .map_err(transport_error)?;
    const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ARTIFACT_BYTES as u64)
    {
        return Err(unsupported("bootstrap artifact exceeds 64 MiB"));
    }
    let mut length = 0usize;
    let mut digest = Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        length = length.saturating_add(chunk.len());
        if length > MAX_ARTIFACT_BYTES {
            return Err(unsupported("bootstrap artifact exceeds 64 MiB"));
        }
        digest.update(chunk);
    }
    if hex::encode(digest.finalize()) != expected_sha256 {
        return Err(unsupported(
            "bootstrap artifact SHA-256 does not match the frozen intent",
        ));
    }
    Ok(())
}

/// Submit exactly one host reset request after its durable intent is committed.
///
/// libredfish retries a non-timeout transport error, including for POSTs. A reset
/// cannot be retried safely after an uncertain result, so this one operation uses
/// a bounded client with retries and redirects disabled. Credentials are supplied
/// by Core's existing reader and never stored here. The BMC certificate policy
/// matches Core's direct BMC pool, which permits self-signed BMC certificates.
pub async fn dispatch_reset_once(
    access: &BmcAccessInfo,
    credentials: Credentials,
    system_uri: &str,
    power_on: bool,
) -> Result<(), RedfishError> {
    local_path(system_uri)?;
    let mut url = url::Url::parse("https://localhost")
        .map_err(|_| unsupported("could not construct BMC reset URL"))?;
    url.set_host(Some(&access.host))
        .map_err(|_| unsupported("invalid BMC host for reset"))?;
    url.set_port(access.port)
        .map_err(|_| unsupported("invalid BMC port for reset"))?;
    url.set_path(&format!("{system_uri}/Actions/ComputerSystem.Reset"));
    let Credentials::UsernamePassword { username, password } = credentials;
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(transport_error)?;
    client
        .post(url)
        .basic_auth(username, Some(password))
        .json(&json!({"ResetType": if power_on { "On" } else { "ForceRestart" }}))
        .send()
        .await
        .map_err(transport_error)?
        .error_for_status()
        .map_err(transport_error)?;
    Ok(())
}

fn link<'a>(resource: &'a Value, name: &str) -> Option<&'a str> {
    resource.get(name)?.get("@odata.id")?.as_str()
}

fn allowed_when_present(boot: &Value, key: &str, required: &str) -> bool {
    boot.get(key).is_none_or(|value| {
        value
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(required)))
    })
}

fn local_path(uri: &str) -> Result<&str, RedfishError> {
    uri.strip_prefix("/redfish/v1/")
        .filter(|path| {
            !path.is_empty() && !path.contains("..") && !path.contains('?') && !path.contains('#')
        })
        .ok_or_else(|| unsupported("BMC advertised a non-local Redfish URI"))
}

fn unsupported(message: &str) -> RedfishError {
    RedfishError::NotSupported(message.to_owned())
}

// Core and libredfish currently use different reqwest releases. Convert without
// carrying the request URL or response body into persisted diagnostics.
fn transport_error(error: reqwest::Error) -> RedfishError {
    let message = if let Some(status) = error.status() {
        format!("request returned HTTP {}", status.as_u16())
    } else if error.is_timeout() {
        "request timed out".to_owned()
    } else {
        "request failed; check connectivity, credentials and TLS".to_owned()
    };
    RedfishError::GenericError { error: message }
}
