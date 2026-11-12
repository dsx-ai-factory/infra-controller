/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Standard Redfish resources drivers of any capability reach: the selected
//! system, chassis, and the BIOS with its attribute reads and writes.

use std::collections::BTreeMap;

use bmc_platform::{BiosSettings, DriverOutcome, OpCx, PlatformError};
use nv_redfish::chassis::Chassis;
use nv_redfish::computer_system::{AttributesUpdate, Bios, BiosUpdate};
use nv_redfish::core::{Bmc, DynamicProperties, EdmPrimitiveType, ModificationResponse};
use nv_redfish::schema::bios::Bios as BiosSchema;
use serde_json::Value;

/// Attribute values keyed by name, from `(name, value)` pairs.
pub(crate) fn attribute_map<const N: usize>(
    entries: [(&str, Value); N],
) -> BTreeMap<String, Value> {
    entries
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect()
}

/// The BIOS update writing `attributes`.
pub(crate) fn bios_update(
    attributes: &BTreeMap<String, Value>,
) -> Result<BiosUpdate, PlatformError> {
    Ok(BiosUpdate::builder()
        .with_attributes(attributes_update(attributes)?)
        .build())
}

/// The entries of `attributes` whose `pending` value, or `current` value when
/// none is pending, differs.
fn unstaged(
    attributes: &BTreeMap<String, Value>,
    current: &BTreeMap<String, Value>,
    pending: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    attributes
        .iter()
        .filter(|(key, value)| pending.get(*key).or_else(|| current.get(*key)) != Some(value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Converts the BIOS `Attributes` object into plain JSON values, skipping null attributes.
pub(crate) fn bios_attributes(bios: &BiosSchema) -> BTreeMap<String, Value> {
    bios.attributes
        .as_ref()
        .map(|attributes| {
            attributes
                .dynamic_properties
                .iter()
                .filter_map(|(key, value)| {
                    let value = match value.as_ref()? {
                        EdmPrimitiveType::String(value) => Value::String(value.clone()),
                        EdmPrimitiveType::Bool(value) => Value::Bool(*value),
                        EdmPrimitiveType::Integer(value) => Value::from(*value),
                        EdmPrimitiveType::Decimal(value) => Value::from(*value),
                    };
                    Some((key.clone(), value))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Converts plain JSON attribute values into the dynamic BIOS attribute update.
fn attributes_update(
    attributes: &BTreeMap<String, Value>,
) -> Result<AttributesUpdate, PlatformError> {
    Ok(AttributesUpdate::builder()
        .with_dynamic_properties(dynamic_properties(attributes)?)
        .build())
}

/// Converts plain JSON attribute values into dynamic attribute properties;
/// attribute registries allow only primitive values.
pub(crate) fn dynamic_properties(
    attributes: &BTreeMap<String, Value>,
) -> Result<DynamicProperties<EdmPrimitiveType>, PlatformError> {
    attributes
        .iter()
        .map(|(key, value)| {
            let value = match value {
                Value::Null => None,
                Value::String(value) => Some(EdmPrimitiveType::String(value.clone())),
                Value::Bool(value) => Some(EdmPrimitiveType::Bool(*value)),
                Value::Number(number) => Some(
                    number
                        .as_i64()
                        .map(EdmPrimitiveType::Integer)
                        .or_else(|| number.as_f64().map(EdmPrimitiveType::Decimal))
                        .ok_or_else(|| PlatformError::InvalidResponse {
                            message: format!("attribute {key} is out of range: {number}"),
                        })?,
                ),
                Value::Array(_) | Value::Object(_) => {
                    return Err(PlatformError::InvalidResponse {
                        message: format!("attribute {key} is not a primitive value"),
                    });
                }
            };
            Ok((key.clone(), value))
        })
        .collect()
}

/// Standard Redfish resources and BIOS attribute mechanics shared across capabilities.
pub(crate) trait RedfishResourcesExt<B: Bmc> {
    /// Every chassis the BMC lists; `Unsupported` when it has no chassis collection.
    async fn all_chassis(&self) -> Result<Vec<Chassis<B>>, PlatformError>;

    /// The chassis with id `chassis_id`; `None` when the BMC does not list it.
    async fn chassis_by_id(&self, chassis_id: &str) -> Result<Option<Chassis<B>>, PlatformError>;

    /// Returns the BIOS resource of the selected ComputerSystem.
    async fn bios(&self) -> Result<Bios<B>, PlatformError>;

    /// The attributes the BIOS currently runs with, excluding nulls.
    async fn current_bios_settings(&self) -> Result<BiosSettings, PlatformError>;
    /// Stages, on the pending-settings resource, the entries of `attributes` the
    /// BIOS would not hold after the next reset, judged by the staged value or,
    /// when nothing is staged, the current one. Repeating a stage writes nothing.
    async fn stage_bios_attributes(
        &self,
        attributes: &BTreeMap<String, Value>,
    ) -> Result<DriverOutcome, PlatformError>;

    /// The entries of `attributes` the BIOS would not hold after the next
    /// reset, judged as [`Self::stage_bios_attributes`] judges them.
    async fn unstaged_bios_attributes(
        &self,
        attributes: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, PlatformError>;

    /// Writes every entry of `attributes` to the pending-settings resource without
    /// comparing current or staged values, for attributes that cannot be read back.
    async fn write_bios_attributes(
        &self,
        attributes: &BTreeMap<String, Value>,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Writes `body` to the BIOS pending-settings resource and returns the
    /// response, whose task location callers such as Dell track as a job.
    async fn update_bios_settings(
        &self,
        body: &BiosUpdate,
    ) -> Result<ModificationResponse<Bios<B>>, PlatformError>;
}

impl<B: Bmc> RedfishResourcesExt<B> for OpCx<'_, B> {
    async fn all_chassis(&self) -> Result<Vec<Chassis<B>>, PlatformError> {
        self.service_root()
            .chassis()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .members()
            .await
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn chassis_by_id(&self, chassis_id: &str) -> Result<Option<Chassis<B>>, PlatformError> {
        let links = self
            .service_root()
            .chassis_links()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        let Some(link) = links
            .iter()
            .find(|link| link.odata_id().last_segment() == Some(chassis_id))
        else {
            // A BMC whose member URIs do not end in the Id is still found by
            // reading every member.
            return Ok(self
                .all_chassis()
                .await?
                .into_iter()
                .find(|chassis| chassis.raw().id == chassis_id));
        };
        link.upgrade::<Chassis<B>>()
            .await
            .map(Some)
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn bios(&self) -> Result<Bios<B>, PlatformError> {
        self.system()
            .await?
            .bios()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)
    }

    async fn current_bios_settings(&self) -> Result<BiosSettings, PlatformError> {
        Ok(BiosSettings {
            attributes: bios_attributes(&self.bios().await?.raw()),
        })
    }

    async fn stage_bios_attributes(
        &self,
        attributes: &BTreeMap<String, Value>,
    ) -> Result<DriverOutcome, PlatformError> {
        if attributes.is_empty() {
            return Ok(DriverOutcome::complete());
        }
        let bios = self.bios().await?;
        let current = bios_attributes(&bios.raw());
        let settings = bios
            .settings()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        let staged = unstaged(attributes, &current, &bios_attributes(&settings.raw()));
        if staged.is_empty() {
            return Ok(DriverOutcome::complete());
        }
        settings
            .update(&bios_update(&staged)?)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn unstaged_bios_attributes(
        &self,
        attributes: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, PlatformError> {
        let bios = self.bios().await?;
        let settings = bios
            .settings()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        Ok(unstaged(
            attributes,
            &bios_attributes(&bios.raw()),
            &bios_attributes(&settings.raw()),
        ))
    }

    async fn write_bios_attributes(
        &self,
        attributes: &BTreeMap<String, Value>,
    ) -> Result<DriverOutcome, PlatformError> {
        self.update_bios_settings(&bios_update(attributes)?)
            .await
            .map(DriverOutcome::from)
    }

    async fn update_bios_settings(
        &self,
        body: &BiosUpdate,
    ) -> Result<ModificationResponse<Bios<B>>, PlatformError> {
        self.bios()
            .await?
            .settings()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .update(body)
            .await
            .map_err(|error| self.map_redfish_error(error))
    }
}

#[cfg(test)]
mod tests {
    use bmc_platform::PlatformIdentity;
    use serde_json::json;

    use super::*;

    /// Without an exploration report the identity names no system or
    /// manager, so drivers act on the ones exploration would choose: the host
    /// that reports a BIOS rather than a baseboard listed before it, and the
    /// manager linked to that host.
    #[tokio::test]
    async fn an_unexplored_bmc_resolves_its_host_system_and_manager() {
        let bmc = bmc_mock::test_support::dgx_gb300_bmc().await;
        let identity = PlatformIdentity::default();
        let cx = OpCx::new(bmc.bmc.as_ref(), bmc.service_root.as_ref(), &identity);

        let system = cx.system().await.expect("the primary system resolves");
        let manager = cx.manager().await.expect("its manager resolves");

        assert_eq!(system.raw().id, "System_0");
        assert_eq!(manager.raw().id, "BMC_0");
    }

    #[test]
    fn attribute_updates_serialize_as_the_plain_json_values() {
        let attributes = BTreeMap::from([
            ("Mode".to_string(), json!("Restricted")),
            ("Enabled".to_string(), json!(true)),
            ("Count".to_string(), json!(4)),
        ]);
        let body = bios_update(&attributes).expect("primitive values");
        assert_eq!(
            serde_json::to_value(&body).expect("body serializes"),
            json!({"Attributes": {"Mode": "Restricted", "Enabled": true, "Count": 4}})
        );
        assert!(bios_update(&attribute_map([("List", json!([1]))])).is_err());
    }
}
