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

use bmc_mock::{HardwareType, RackPlacement, RackType};
use carbide_uuid::rack::{RackGroupId, RackId, RackProfileId};
use model::expected_rack::derive_rack_profile_id;
use model::rack_type::RackCapabilityType;
use rpc::forge::{ExpectedRackGroup, ExpectedRackGroupMember, ExpectedRackGroupRack};
use serde::Serialize;

use crate::api_client::ExpectedRecord;
use crate::status::DeviceStatus;

/// Fabric protocol declared for the group. GB200 and GB300 racks use
/// fifth-generation NVLink.
const RACK_GROUP_PROTOCOL: &str = "NVLINK_V5";

/// Group topology nico-api derives the rack profile from.
fn topology(rack_type: RackType) -> &'static str {
    match rack_type {
        RackType::WiwynnGb200Nvl72 => "gb200_nvl72r1_c2g4",
        RackType::LenovoGb300Nvl72 => "gb300_nvl72r1_c2g4",
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RackMemberRegistration {
    pub placement: RackPlacement,
    pub hardware_type: HardwareType,
    pub machine_config_section: String,
}

impl RackMemberRegistration {
    /// Member declared for this unit, identified by its machine config section.
    fn expected_member(&self) -> eyre::Result<ExpectedRackGroupMember> {
        use RackCapabilityType::{Compute, PowerShelf, Switch};

        let (device_type, manufacturer) = match self.hardware_type {
            HardwareType::WiwynnGB200Nvl => (Compute, "WIWYNN"),
            HardwareType::LenovoGB300Nvl => (Compute, "Lenovo"),
            HardwareType::NvidiaSwitchNd5200Ld | HardwareType::NvidiaSwitchN5700Ld => {
                (Switch, "NVIDIA")
            }
            HardwareType::LiteOnPowerShelf => (PowerShelf, "LiteOn"),
            HardwareType::DeltaPowerShelf => (PowerShelf, "Delta"),
            HardwareType::DellPowerEdgeR750
            | HardwareType::DellPowerEdgeR760Bf4
            | HardwareType::NvidiaDgxGb300
            | HardwareType::SupermicroGb300Nvl
            | HardwareType::NvidiaDgxVr
            | HardwareType::NvidiaDgxH100
            | HardwareType::GenericAmi
            | HardwareType::HpeProliantDl380aGen11
            | HardwareType::GenericSupermicro => {
                eyre::bail!("{} is not a rack member hardware type", self.hardware_type)
            }
        };
        Ok(ExpectedRackGroupMember {
            r#type: device_type.to_string(),
            manufacturer: manufacturer.to_string(),
            id: self.machine_config_section.clone(),
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RackRegistration {
    pub rack_id: RackId,
    pub rack_profile_id: RackProfileId,
    pub rack_type: RackType,
    pub version: u32,
    pub members: Vec<RackMemberRegistration>,
}

impl RackRegistration {
    /// Expected rack group declared when no group declares this rack yet:
    /// one group per rack, keyed by the rack ID.
    pub(crate) fn expected_rack_group(&self) -> eyre::Result<ExpectedRackGroup> {
        let members = self
            .members
            .iter()
            .map(RackMemberRegistration::expected_member)
            .collect::<eyre::Result<Vec<_>>>()?;
        Ok(ExpectedRackGroup {
            rack_group_id: Some(RackGroupId::new(self.rack_id.as_str())),
            topology: topology(self.rack_type).to_string(),
            protocol: RACK_GROUP_PROTOCOL.to_string(),
            metadata: None,
            racks: vec![ExpectedRackGroupRack {
                rack_id: Some(self.rack_id.clone()),
                members,
            }],
        })
    }

    /// Rack profile nico-api derives for this rack from `group`. A group
    /// read back from nico-api may predate protocols and carry none.
    pub(crate) fn derived_rack_profile_id(
        &self,
        group: ExpectedRackGroup,
    ) -> eyre::Result<RackProfileId> {
        let group = rpc::model::expected_rack_group::stored_expected_rack_group(group)?;
        derive_rack_profile_id(&group, &self.rack_id).map_err(eyre::Report::msg)
    }

    /// Registration record for this rack. `existing` is the group that
    /// already declares the rack, if any; otherwise the rack declares its
    /// own group. Fails when `rack_profile_id` is not the profile nico-api
    /// derives from that group.
    pub(crate) fn expected_record(
        &self,
        existing: Option<&ExpectedRackGroup>,
    ) -> eyre::Result<ExpectedRecord> {
        let (group, declared) = match existing {
            Some(group) => (group.clone(), None),
            None => {
                let group = self.expected_rack_group()?;
                (group.clone(), Some(group))
            }
        };
        let rack_group_id = group
            .rack_group_id
            .as_ref()
            .map(RackGroupId::to_string)
            .unwrap_or_default();
        let derived = self.derived_rack_profile_id(group)?;
        eyre::ensure!(
            self.rack_profile_id == derived,
            "rack {} configures rack_profile_id {}, but nico-api derives {derived} from expected rack group {rack_group_id}",
            self.rack_id,
            self.rack_profile_id
        );
        Ok(ExpectedRecord::Rack {
            rack_id: self.rack_id.clone(),
            rack_profile_id: self.rack_profile_id.clone(),
            group: declared,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RackMemberRef {
    pub placement: RackPlacement,
    pub device_index: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct RackInstance {
    pub rack_id: RackId,
    pub rack_type: RackType,
    pub version: u32,
    pub members: Vec<RackMemberRef>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RacksStatusResponse {
    pub racks: Vec<RackStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RackStatus {
    pub rack_id: String,
    pub rack_type: RackType,
    pub version: u32,
    pub members: Vec<RackMemberStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RackMemberStatus {
    pub position: u8,
    #[serde(flatten)]
    pub device: DeviceStatus,
}
