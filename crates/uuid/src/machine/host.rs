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

use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::str::FromStr;

use super::{
    InvalidMachineType, MachineId, MachineIdSubtype, MachineIdSubtypeTrait, MachineType,
    PredictedHostMachineId, StableHostMachineId,
};

/// A machine ID that identifies either a stable or predicted host.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct HostMachineId(pub(super) MachineId);

impl_prost_message_for_machine_id!(HostMachineId, MachineType::Host);

impl MachineIdSubtypeTrait for HostMachineId {
    fn machine_type(&self) -> MachineType {
        match self.host_machine_id_subtype() {
            HostMachineIdSubtype::Stable(_) => MachineType::Host,
            HostMachineIdSubtype::Predicted(_) => MachineType::PredictedHost,
        }
    }

    fn as_machine_id(&self) -> &MachineId {
        HostMachineId::as_machine_id(self)
    }

    fn machine_id_subtype(&self) -> MachineIdSubtype {
        match Self::host_machine_id_subtype(self) {
            HostMachineIdSubtype::Stable(id) => MachineIdSubtype::StableHost(id),
            HostMachineIdSubtype::Predicted(id) => MachineIdSubtype::PredictedHost(id),
        }
    }
}

/// Actual type-safe variants of a host machine ID, stable (fm100h) or predicted (fm100p)
pub enum HostMachineIdSubtype {
    Stable(StableHostMachineId),
    Predicted(PredictedHostMachineId),
}

impl HostMachineId {
    /// Returns the underlying machine ID.
    pub fn as_machine_id(&self) -> &MachineId {
        &self.0
    }

    /// Returns whether this ID identifies a stable host.
    pub fn is_stable_host(&self) -> bool {
        matches!(self.0.machine_type(), MachineType::Host)
    }

    pub fn host_machine_id_subtype(&self) -> HostMachineIdSubtype {
        match self.0.ty {
            MachineType::Host => {
                // SAFETY: subtypes must convert if the type matches
                HostMachineIdSubtype::Stable(StableHostMachineId::try_from(self.0).unwrap())
            }
            MachineType::PredictedHost => {
                // SAFETY: subtypes must convert if the type matches
                HostMachineIdSubtype::Predicted(PredictedHostMachineId::try_from(self.0).unwrap())
            }
            // SAFETY: code in this module ensures this can't happen
            MachineType::Dpu => {
                unreachable!("BUG: HostMachineId was constructed from a DPU machine ID")
            }
        }
    }
}

impl Deref for HostMachineId {
    type Target = MachineId;
    fn deref(&self) -> &MachineId {
        &self.0
    }
}

impl TryFrom<MachineId> for HostMachineId {
    type Error = InvalidMachineType;

    fn try_from(id: MachineId) -> Result<Self, Self::Error> {
        match id.machine_type() {
            MachineType::Host | MachineType::PredictedHost => Ok(Self(id)),
            MachineType::Dpu => Err(InvalidMachineType {
                expected: "stable or predicted host ID",
                actual: id,
            }),
        }
    }
}

impl TryFrom<&MachineId> for HostMachineId {
    type Error = InvalidMachineType;

    fn try_from(id: &MachineId) -> Result<Self, Self::Error> {
        Self::try_from(*id)
    }
}

impl FromStr for HostMachineId {
    type Err = crate::machine::MachineIdSubtypeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = MachineId::from_str(value)?;
        Ok(Self::try_from(id)?)
    }
}

impl From<HostMachineId> for MachineId {
    fn from(id: HostMachineId) -> Self {
        id.0
    }
}

impl From<&HostMachineId> for MachineId {
    fn from(id: &HostMachineId) -> Self {
        id.0
    }
}

impl Display for HostMachineId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl<'de> serde::Deserialize<'de> for HostMachineId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let id = <MachineId as serde::Deserialize>::deserialize(deserializer)?;
        Self::try_from(id).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "sqlx")]
impl_sqlx_for_machine_id_newtype!(HostMachineId);
