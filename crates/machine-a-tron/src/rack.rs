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
use model::expected_rack_group::{
    ExpectedRackGroup, ExpectedRackGroupMember, ExpectedRackGroupRack, RackGroupTopology,
};
use model::rack_type::RackCapabilityType;
use serde::Serialize;

use crate::status::{DeviceKind, DeviceStatus};

#[derive(Clone, Debug)]
pub(crate) struct RackMemberRegistration {
    pub placement: RackPlacement,
    pub hardware_type: HardwareType,
    pub machine_config_section: String,
}

#[derive(Clone, Debug)]
pub(crate) struct RackRegistration {
    pub rack_id: RackId,
    /// The profile nico-api derives for this rack from `rack_group`, kept so
    /// a restart can detect an expected rack that was registered differently.
    pub rack_profile_id: RackProfileId,
    pub rack_type: RackType,
    pub version: u32,
    pub members: Vec<RackMemberRegistration>,
    /// The expected rack group that declares this rack. nico-api requires the
    /// group before it accepts the expected rack, and derives the rack's
    /// profile from the group's topology and member manufacturers. One group
    /// holds one rack, so the group is also the rack's NVLink domain identity.
    pub rack_group: ExpectedRackGroup,
}

impl RackRegistration {
    /// Builds the registration for one simulated rack, declaring its expected
    /// rack group from the rack design and deriving the profile the same way
    /// nico-api does, so the two cannot disagree.
    pub(crate) fn new(
        rack_id: RackId,
        rack_type: RackType,
        version: u32,
        members: Vec<RackMemberRegistration>,
    ) -> eyre::Result<Self> {
        let rack_group = expected_rack_group(&rack_id, rack_type, &members)?;
        let rack_profile_id = derive_rack_profile_id(&rack_group, &rack_id)
            .map_err(|error| eyre::eyre!("rack {rack_id}: {error}"))?;
        Ok(Self {
            rack_id,
            rack_profile_id,
            rack_type,
            version,
            members,
            rack_group,
        })
    }
}

/// The expected rack group declaring one simulated rack: one member per rack
/// unit, typed and attributed the way nico-api's profile derivation reads them.
pub(crate) fn expected_rack_group(
    rack_id: &RackId,
    rack_type: RackType,
    members: &[RackMemberRegistration],
) -> eyre::Result<ExpectedRackGroup> {
    let group_members = members
        .iter()
        .map(|member| {
            let hardware_type = member.hardware_type;
            let device_type = match DeviceKind::from(hardware_type) {
                DeviceKind::Machine => RackCapabilityType::Compute,
                DeviceKind::Switch => RackCapabilityType::Switch,
                DeviceKind::PowerShelf => RackCapabilityType::PowerShelf,
                DeviceKind::Dpu => {
                    eyre::bail!("rack {rack_id} lists a DPU as a rack member")
                }
            };
            let manufacturer = rack_member_manufacturer(hardware_type).ok_or_else(|| {
                eyre::eyre!(
                    "rack {rack_id} contains {hardware_type}, which has no rack group manufacturer"
                )
            })?;
            Ok(ExpectedRackGroupMember {
                device_type,
                manufacturer: manufacturer.to_string(),
                id: format!("{rack_id}-unit-{:02}", member.placement.position()),
            })
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    Ok(ExpectedRackGroup {
        rack_group_id: RackGroupId::new(format!("group-{rack_id}")),
        topology: RackGroupTopology::new(rack_group_topology(rack_type)),
        racks: vec![ExpectedRackGroupRack {
            rack_id: rack_id.clone(),
            members: group_members,
        }],
        metadata: Default::default(),
    })
}

/// The NVLink topology an expected rack group declares for a rack design.
/// nico-api uppercases it to form the first part of the derived profile ID,
/// so these match the `rack_profiles` the nico-api chart ships.
fn rack_group_topology(rack_type: RackType) -> &'static str {
    match rack_type {
        RackType::WiwynnGb200Nvl72 => "gb200_nvl72r1_c2g4",
        RackType::LenovoGb300Nvl72 => "gb300_nvl72r1_c2g4",
    }
}

/// The manufacturer an expected rack group declares for rack hardware, in
/// the spelling the nico-api chart's `rack_profiles` use for the vendor.
/// Hardware that no rack design places returns `None`.
fn rack_member_manufacturer(hardware_type: HardwareType) -> Option<&'static str> {
    match hardware_type {
        HardwareType::WiwynnGB200Nvl => Some("WIWYNN"),
        HardwareType::LenovoGB300Nvl => Some("Lenovo"),
        HardwareType::NvidiaSwitchNd5200Ld | HardwareType::NvidiaSwitchN5700Ld => Some("NVIDIA"),
        HardwareType::LiteOnPowerShelf => Some("LiteOn"),
        HardwareType::DeltaPowerShelf => Some("Delta"),
        _ => None,
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
