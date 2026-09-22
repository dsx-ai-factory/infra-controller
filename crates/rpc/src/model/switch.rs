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

use carbide_uuid::switch::SwitchId;
use mac_address::MacAddress;
use model::switch::{
    FabricManagerConfig, FabricManagerState, NewSwitch, Switch, SwitchConfig, SwitchSearchFilter,
    derive_switch_aggregate_health, state_sla,
};

use crate::errors::RpcDataConversionError;
use crate::forge::{self as rpc, LifecycleStatus};

impl TryFrom<rpc::SwitchCreationRequest> for NewSwitch {
    type Error = RpcDataConversionError;
    fn try_from(value: rpc::SwitchCreationRequest) -> Result<Self, Self::Error> {
        let conf = match value.config {
            Some(c) => c,
            None => {
                return Err(RpcDataConversionError::InvalidArgument(
                    "Switch configuration is empty".to_string(),
                ));
            }
        };

        let switch_uuid: Option<uuid::Uuid> = value
            .id
            .as_ref()
            .map(|rpc_uuid| {
                rpc_uuid
                    .try_into()
                    .map_err(|_| RpcDataConversionError::InvalidSwitchId(rpc_uuid.to_string()))
            })
            .transpose()?;

        let id = match switch_uuid {
            Some(v) => SwitchId::from(v),
            None => uuid::Uuid::new_v4().into(),
        };

        let config = SwitchConfig::try_from(conf)?;

        Ok(NewSwitch {
            id,
            config,
            bmc_mac_address: None,
            metadata: None,
            rack_id: None,
            slot_number: value.placement_in_rack.as_ref().and_then(|p| p.slot_number),
            tray_index: value.placement_in_rack.as_ref().and_then(|p| p.tray_index),
        })
    }
}

fn to_rpc_fabric_manager_state(state: FabricManagerState) -> i32 {
    match state {
        FabricManagerState::Ok => rpc::FabricManagerState::Ok as i32,
        FabricManagerState::NotOk => rpc::FabricManagerState::NotOk as i32,
        FabricManagerState::Unknown => rpc::FabricManagerState::Unknown as i32,
    }
}

impl TryFrom<rpc::SwitchConfig> for SwitchConfig {
    type Error = RpcDataConversionError;

    fn try_from(conf: rpc::SwitchConfig) -> Result<Self, Self::Error> {
        Ok(SwitchConfig {
            name: conf.name,
            enable_nmxc: conf.enable_nmxc,
            fabric_manager_config: Some(FabricManagerConfig {
                config_map: conf.fabric_manager_config.unwrap_or_default().config_map,
            }),
        })
    }
}

impl TryFrom<Switch> for rpc::Switch {
    type Error = RpcDataConversionError;

    fn try_from(src: Switch) -> Result<Self, Self::Error> {
        let health = derive_switch_aggregate_health(&src.health_reports);
        let fabric_manager_status = src
            .fabric_manager_status
            .as_ref()
            .map(|status| status.display_status().to_string());
        let fabric_manager_status_details =
            src.fabric_manager_status
                .as_ref()
                .map(|status| rpc::FabricManagerStatus {
                    fabric_manager_state: to_rpc_fabric_manager_state(
                        status.fabric_manager_state.clone(),
                    ),
                    addition_info: status.addition_info.clone(),
                    reason: status.reason.clone(),
                    error_message: status.error_message.clone(),
                });
        let health_sources = src
            .health_reports
            .iter()
            .map(|(hr, m)| rpc::HealthSourceOrigin {
                mode: m as i32,
                source: hr.source.clone(),
            })
            .collect();

        let sla = state_sla(&src.controller_state.value, &src.controller_state.version);
        let lifecycle = LifecycleStatus {
            state: serde_json::to_string(&src.controller_state.value).unwrap_or_default(),
            version: src.controller_state.version.version_string(),
            state_reason: src.controller_state_outcome.map(Into::into),
            sla: Some(sla.clone().into()),
        };
        let controller_state = lifecycle.state.clone();
        let status = Some(
            match (
                src.status,
                fabric_manager_status,
                fabric_manager_status_details,
            ) {
                (Some(s), fabric_manager_status, fabric_manager_status_details) => {
                    rpc::SwitchStatus {
                        state_reason: lifecycle.state_reason.clone(),
                        state_sla: Some(sla.into()),
                        switch_name: Some(s.switch_name),
                        power_state: Some(s.power_state),
                        health_status: Some(s.health_status),
                        controller_state: Some(lifecycle.state.clone()),
                        health: Some(health.into()),
                        health_sources,
                        lifecycle: Some(lifecycle),
                        fabric_manager_status,
                        fabric_manager_status_details,
                    }
                }
                (None, fabric_manager_status, fabric_manager_status_details) => rpc::SwitchStatus {
                    state_reason: lifecycle.state_reason.clone(),
                    state_sla: Some(sla.into()),
                    switch_name: None,
                    power_state: None,
                    health_status: None,
                    controller_state: Some(lifecycle.state.clone()),
                    health: Some(health.into()),
                    health_sources,
                    lifecycle: Some(lifecycle),
                    fabric_manager_status,
                    fabric_manager_status_details,
                },
            },
        );

        let placement_in_rack = Some(rpc::PlacementInRack {
            slot_number: src.slot_number,
            tray_index: src.tray_index,
        });
        let config = rpc::SwitchConfig {
            name: src.config.name,
            fabric_manager_config: Some(rpc::FabricManagerConfig {
                config_map: src
                    .config
                    .fabric_manager_config
                    .unwrap_or_default()
                    .config_map,
            }),
            enable_nmxc: src.config.enable_nmxc,
        };

        let deleted = src.deleted.map(Into::into);
        let state_version = src.controller_state.version.to_string();
        Ok(rpc::Switch {
            id: Some(src.id),
            config: Some(config),
            status,
            deleted,
            controller_state,
            bmc_info: src.bmc_info.map(Into::into),
            nvos_info: None,
            nvlink_domain_uuid: src.nvlink_domain_uuid,
            state_version,
            metadata: Some(src.metadata.into()),
            version: src.version.version_string(),
            rack_id: src.rack_id,
            placement_in_rack,
            is_primary: src.is_primary,
        })
    }
}

impl From<rpc::SwitchSearchFilter> for SwitchSearchFilter {
    fn from(filter: rpc::SwitchSearchFilter) -> Self {
        SwitchSearchFilter {
            rack_id: filter.rack_id,
            deleted: model::DeletedFilter::from(filter.deleted),
            controller_state: filter.controller_state,
            bmc_mac: filter.bmc_mac.and_then(|m| m.parse::<MacAddress>().ok()),
            nvos_mac: filter.nvos_mac.and_then(|m| m.parse::<MacAddress>().ok()),
            only_with_health_alert: filter.only_with_health_alert,
        }
    }
}

#[cfg(test)]
mod tests {
    use config_version::{ConfigVersion, Versioned};
    use model::controller_outcome::PersistentStateHandlerOutcome;
    use model::metadata::Metadata;
    use model::switch::{FabricManagerStatus, SwitchControllerState, SwitchStatus};

    use super::*;

    #[test]
    fn try_from_switch_populates_state_reason() {
        let nvlink_domain_uuid = "9f4b45ec-705a-4af4-89f7-a112bc9c8f4e"
            .parse()
            .expect("valid NVLink domain UUID");

        let switch = Switch {
            id: SwitchId::from(uuid::Uuid::new_v4()),
            config: SwitchConfig {
                name: "test-switch".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            status: Some(SwitchStatus {
                switch_name: "test-switch".to_string(),
                power_state: "on".to_string(),
                health_status: "ok".to_string(),
            }),
            deleted: None,
            bmc_mac_address: None,
            bmc_info: None,
            bmc_credential_rotation_requested: false,
            decommission_requested: false,
            controller_state: Versioned::new(
                SwitchControllerState::Ready,
                config_version::ConfigVersion::initial(),
            ),
            controller_state_outcome: Some(PersistentStateHandlerOutcome::Transition {
                source_ref: None,
            }),
            switch_maintenance_requested: None,
            switch_reprovisioning_requested: None,
            firmware_upgrade_status: None,
            nvos_update_status: None,
            fabric_manager_status: Some(FabricManagerStatus {
                fabric_manager_state: FabricManagerState::Ok,
                addition_info: Some("CONTROL_PLANE_STATE_CONFIGURED".to_string()),
                reason: Some(String::new()),
                error_message: None,
            }),
            nvlink_domain_uuid: Some(nvlink_domain_uuid),
            metadata: Metadata::default(),
            version: ConfigVersion::initial(),
            is_primary: true,
            rack_id: None,
            slot_number: Some(1),
            tray_index: Some(2),
            health_reports: Default::default(),
        };

        let rpc_switch: rpc::Switch = switch.try_into().unwrap();
        let status = rpc_switch.status.expect("status should be Some");
        assert!(
            status.state_reason.is_some(),
            "state_reason should be populated from controller_state_outcome"
        );
        assert!(status.state_sla.is_some(), "state_sla should be populated");
        assert_eq!(status.power_state, Some("on".to_string()));
        assert_eq!(status.health_status, Some("ok".to_string()));
        assert_eq!(status.fabric_manager_status, Some("running".to_string()));

        let details = status
            .fabric_manager_status_details
            .expect("fabric_manager_status_details should be populated");

        assert_eq!(
            details.fabric_manager_state,
            rpc::FabricManagerState::Ok as i32
        );
        assert_eq!(
            details.addition_info,
            Some("CONTROL_PLANE_STATE_CONFIGURED".to_string())
        );
        assert!(rpc_switch.is_primary);
        assert_eq!(rpc_switch.nvlink_domain_uuid, Some(nvlink_domain_uuid));
    }

    #[test]
    fn try_from_switch_without_status_still_has_state_reason() {
        let switch = Switch {
            id: SwitchId::from(uuid::Uuid::new_v4()),
            config: SwitchConfig {
                name: "test-switch".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            status: None,
            deleted: None,
            bmc_mac_address: None,
            bmc_info: None,
            bmc_credential_rotation_requested: false,
            decommission_requested: false,
            controller_state: Versioned::new(
                SwitchControllerState::Created,
                config_version::ConfigVersion::initial(),
            ),
            controller_state_outcome: Some(PersistentStateHandlerOutcome::Wait {
                reason: "waiting for something".to_string(),
                source_ref: None,
            }),
            switch_maintenance_requested: None,
            switch_reprovisioning_requested: None,
            firmware_upgrade_status: None,
            nvos_update_status: None,
            fabric_manager_status: None,
            nvlink_domain_uuid: None,
            metadata: Metadata::default(),
            version: ConfigVersion::initial(),
            is_primary: false,
            rack_id: None,
            slot_number: None,
            tray_index: None,
            health_reports: Default::default(),
        };

        let rpc_switch: rpc::Switch = switch.try_into().unwrap();
        let status = rpc_switch
            .status
            .expect("status should be Some even when switch.status is None");
        assert!(
            status.state_reason.is_some(),
            "state_reason should be populated even without switch status"
        );
        assert_eq!(status.power_state, None);
        assert_eq!(status.health_status, None);
        assert_eq!(status.fabric_manager_status, None);
        assert_eq!(status.fabric_manager_status_details, None);
        assert!(!rpc_switch.is_primary);
        assert_eq!(rpc_switch.nvlink_domain_uuid, None);
    }
}
