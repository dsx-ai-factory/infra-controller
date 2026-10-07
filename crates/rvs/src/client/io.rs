use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use rpc::forge::{
    GetRackRequest, Instance, InstanceAllocationRequest, InstanceConfig, Label,
    MachineMetadataUpdateRequest, MachinesByIdsRequest, Metadata,
};
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::ApiConfig;
use rpc::protos::forge::{
    InstanceOperatingSystemConfig, InstancesByIdsRequest, MachineSearchConfig,
    instance_operating_system_config,
};

use super::{RackData, TrayData};
use crate::error::RvsError;

/// NICo gRPC client wrapper -- translates gRPC responses into IR types.
pub struct NicoClient {
    inner: ForgeApiClient,
}

impl NicoClient {
    /// Construct from API config.
    pub fn new(api_config: &ApiConfig<'_>) -> Self {
        Self {
            inner: ForgeApiClient::new(api_config),
        }
    }

    /// Fetch all racks from NICo -> Vec<RackData>.
    pub async fn get_racks(&self) -> Result<Vec<RackData>, RvsError> {
        let response = self.inner.get_rack(GetRackRequest { id: None }).await?;
        response.rack.into_iter().map(RackData::try_from).collect()
    }

    /// Update `rv.*` labels on a machine, preserving all non-`rv.*` labels.
    pub async fn update_rv_labels(
        &self,
        tray_id: &MachineId,
        updates: HashMap<String, String>,
    ) -> Result<(), RvsError> {
        let response = self
            .inner
            .find_machines_by_ids(MachinesByIdsRequest {
                machine_ids: vec![*tray_id],
                include_history: false,
            })
            .await?;

        let count = response.machines.len();
        let (1, Some(machine)) = (count, response.machines.into_iter().next()) else {
            return Err(RvsError::UnexpectedMachineCount {
                tray_id: tray_id.to_string(),
                count,
            });
        };

        let metadata = machine.metadata.unwrap_or_default();
        let label_protos = merge_rv_labels(metadata.labels, updates);

        self.inner
            .update_machine_metadata(MachineMetadataUpdateRequest {
                machine_id: Some(*tray_id),
                if_version_match: None,
                metadata: Some(Metadata {
                    name: String::new(),
                    description: String::new(),
                    labels: label_protos,
                }),
            })
            .await?;
        Ok(())
    }

    /// Allocate a validation instance on a single machine.
    ///
    /// The OS is identified by `os_uri` from the scenario file. Until RVS can
    /// resolve the URI to a NICo OS image UUID, `os_image_id` is stubbed with
    /// a nil UUID - the call will fail in production until this is wired up.
    pub async fn allocate_machine_instance(
        &self,
        machine_id: &str,
        os_uri: &str,
    ) -> Result<String, RvsError> {
        let machine_id = machine_id
            .parse()
            .map_err(|error| RvsError::InvalidId(format!("{error}")))?;
        tracing::info!(%os_uri, "validation: allocating instance (os_image_id stubbed)");
        let response = self
            .inner
            .allocate_instance(InstanceAllocationRequest {
                machine_id: Some(machine_id),
                config: Some(InstanceConfig {
                    os: Some(InstanceOperatingSystemConfig {
                        // TODO[#416]: resolve os_uri to a NICo OS image UUID via ListOsImage /
                        //       an external registry lookup. For now, nil UUID is a known
                        //       stub that will be replaced once image resolution is wired.
                        variant: Some(instance_operating_system_config::Variant::OsImageId(
                            rpc::common::Uuid {
                                value: "00000000-0000-0000-0000-000000000000".to_string(),
                            },
                        )),
                        phone_home_enabled: false,
                        run_provisioning_instructions_on_every_boot: false,
                        user_data: None,
                    }),
                    tenant: None,
                    network: None,
                    infiniband: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                }),
                instance_id: None,
                instance_type_id: None,
                metadata: None,
                allow_unhealthy_machine: false,
            })
            .await?;
        Ok(response.id.map(|id| id.to_string()).unwrap_or_default())
    }

    /// Fetch current state of instances by their IDs.
    pub async fn get_instances(&self, instance_ids: &[String]) -> Result<Vec<Instance>, RvsError> {
        let ids = instance_ids
            .iter()
            .map(|id| {
                id.parse()
                    .map_err(|e: uuid::Error| RvsError::InvalidId(e.to_string()))
            })
            .collect::<Result<_, _>>()?;
        let response = self
            .inner
            .find_instances_by_ids(InstancesByIdsRequest { instance_ids: ids })
            .await?;
        Ok(response.instances)
    }

    /// Fetch machines for a rack -> Vec<TrayData>. Chunked at 50.
    pub async fn get_machines(&self, rack: &RackData) -> Result<Vec<TrayData>, RvsError> {
        let id_list = self
            .inner
            .find_machine_ids(MachineSearchConfig {
                rack_id: Some(rack.id.clone()),
                ..Default::default()
            })
            .await?;

        let mut trays = Vec::with_capacity(id_list.machine_ids.len());
        for chunk in id_list.machine_ids.chunks(50) {
            let response = self
                .inner
                .find_machines_by_ids(MachinesByIdsRequest {
                    machine_ids: chunk.to_vec(),
                    include_history: false,
                })
                .await?;

            for machine in response.machines {
                trays.push(TrayData::try_from(machine)?);
            }
        }

        Ok(trays)
    }
}

/// Merge `rv.*` label updates into an existing label list.
///
/// Keeps every non-`rv.*` label from `existing` as-is, preserving the
/// original `Option<String>` value (so `None` stays `None` rather than
/// becoming `Some("")`).
/// Replaces or adds every key from `updates` (all of which are `rv.*`).
/// Drops `rv.*` keys present in `existing` but absent from `updates`.
fn merge_rv_labels(existing: Vec<Label>, updates: HashMap<String, String>) -> Vec<Label> {
    let mut merged: Vec<Label> = existing
        .into_iter()
        .filter(|label| !label.key.starts_with("rv."))
        .collect();
    merged.extend(updates.into_iter().map(|(key, value)| Label {
        key,
        value: Some(value),
    }));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn labels(pairs: &[(&str, Option<&str>)]) -> Vec<Label> {
        pairs
            .iter()
            .map(|(k, v)| Label {
                key: k.to_string(),
                value: v.map(String::from),
            })
            .collect()
    }

    fn find<'a>(labels: &'a [Label], key: &str) -> Option<&'a Label> {
        labels.iter().find(|l| l.key == key)
    }

    #[test]
    fn test_merge_preserves_non_rv_labels() {
        let existing = labels(&[("owner", Some("ops")), ("rv.run-id", Some("old"))]);
        let updates = map(&[("rv.run-id", "new")]);
        let result = merge_rv_labels(existing, updates);
        assert_eq!(result.len(), 2);
        assert_eq!(
            find(&result, "owner").unwrap().value.as_deref(),
            Some("ops")
        );
        assert_eq!(
            find(&result, "rv.run-id").unwrap().value.as_deref(),
            Some("new")
        );
    }

    #[test]
    fn test_merge_drops_stale_rv_keys() {
        let existing = labels(&[("rv.run-id", Some("old")), ("rv.st", Some("pass"))]);
        let updates = map(&[("rv.run-id", "new")]);
        let result = merge_rv_labels(existing, updates);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].key, "rv.run-id");
        assert_eq!(result[0].value.as_deref(), Some("new"));
    }

    #[test]
    fn test_merge_adds_new_rv_keys() {
        let existing = labels(&[("owner", Some("ops"))]);
        let updates = map(&[("rv.run-id", "abc")]);
        let result = merge_rv_labels(existing, updates);
        assert_eq!(result.len(), 2);
        assert_eq!(
            find(&result, "owner").unwrap().value.as_deref(),
            Some("ops")
        );
        assert_eq!(
            find(&result, "rv.run-id").unwrap().value.as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn test_merge_empty_existing() {
        let updates = map(&[("rv.run-id", "abc")]);
        let result = merge_rv_labels(vec![], updates);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].key, "rv.run-id");
        assert_eq!(result[0].value.as_deref(), Some("abc"));
    }

    #[test]
    fn test_merge_empty_updates() {
        let existing = labels(&[("owner", Some("ops")), ("rv.run-id", Some("old"))]);
        let result = merge_rv_labels(existing, map(&[]));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].key, "owner");
        assert_eq!(result[0].value.as_deref(), Some("ops"));
    }

    #[test]
    fn test_merge_preserves_none_value_on_non_rv_labels() {
        // A non-`rv.*` label with `value: None` must stay `None` -- the
        // previous implementation went through `unwrap_or_default()` and
        // silently turned it into `Some("")`.
        let existing = labels(&[("owner", None)]);
        let updates = map(&[("rv.run-id", "new")]);
        let result = merge_rv_labels(existing, updates);
        assert_eq!(find(&result, "owner").unwrap().value, None);
    }
}
