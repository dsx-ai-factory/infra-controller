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
    HostMachineId, InvalidMachineType, MachineId, MachineIdSubtype, MachineIdSubtypeTrait,
    MachineType,
};

/// A machine ID that identifies a predicted host.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PredictedHostMachineId(pub(super) HostMachineId);

impl_prost_message_for_machine_id!(PredictedHostMachineId, MachineType::PredictedHost);

impl MachineIdSubtypeTrait for PredictedHostMachineId {
    fn machine_type(&self) -> MachineType {
        MachineType::PredictedHost
    }

    fn as_machine_id(&self) -> &MachineId {
        PredictedHostMachineId::as_machine_id(self)
    }

    fn machine_id_subtype(&self) -> MachineIdSubtype {
        MachineIdSubtype::PredictedHost(*self)
    }
}

impl PredictedHostMachineId {
    /// Returns the underlying machine ID.
    pub fn as_machine_id(&self) -> &MachineId {
        &self.0.0
    }

    /// Returns the underlying HostMachineId (which is allowed to be either a predicted or stable
    /// host)
    pub fn as_host_machine_id(&self) -> &HostMachineId {
        &self.0
    }
}

impl Deref for PredictedHostMachineId {
    type Target = HostMachineId;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<HostMachineId> for PredictedHostMachineId {
    fn as_ref(&self) -> &HostMachineId {
        &self.0
    }
}

impl TryFrom<MachineId> for PredictedHostMachineId {
    type Error = InvalidMachineType;

    fn try_from(id: MachineId) -> Result<Self, Self::Error> {
        match id.machine_type() {
            MachineType::PredictedHost => Ok(Self(HostMachineId(id))),
            _ => Err(InvalidMachineType {
                expected: "predicted host machine ID",
                actual: id,
            }),
        }
    }
}

impl TryFrom<HostMachineId> for PredictedHostMachineId {
    type Error = InvalidMachineType;

    fn try_from(id: HostMachineId) -> Result<Self, Self::Error> {
        match id.0.ty {
            MachineType::PredictedHost => Ok(Self(id)),
            _ => Err(InvalidMachineType {
                expected: "predicted host machine ID",
                actual: id.0,
            }),
        }
    }
}

impl TryFrom<&MachineId> for PredictedHostMachineId {
    type Error = InvalidMachineType;

    fn try_from(id: &MachineId) -> Result<Self, Self::Error> {
        Self::try_from(*id)
    }
}

impl TryFrom<&HostMachineId> for PredictedHostMachineId {
    type Error = InvalidMachineType;

    fn try_from(id: &HostMachineId) -> Result<Self, Self::Error> {
        Self::try_from(*id)
    }
}

impl FromStr for PredictedHostMachineId {
    type Err = crate::machine::MachineIdSubtypeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = MachineId::from_str(value)?;
        Ok(Self::try_from(id)?)
    }
}

impl From<PredictedHostMachineId> for HostMachineId {
    fn from(id: PredictedHostMachineId) -> Self {
        id.0
    }
}

impl From<PredictedHostMachineId> for MachineId {
    fn from(id: PredictedHostMachineId) -> Self {
        id.0.into()
    }
}

impl From<&PredictedHostMachineId> for HostMachineId {
    fn from(id: &PredictedHostMachineId) -> Self {
        id.0
    }
}

impl From<&PredictedHostMachineId> for MachineId {
    fn from(id: &PredictedHostMachineId) -> Self {
        id.0.into()
    }
}

impl Display for PredictedHostMachineId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl<'de> serde::Deserialize<'de> for PredictedHostMachineId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let id = <MachineId as serde::Deserialize>::deserialize(deserializer)?;
        Self::try_from(id).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "sqlx")]
impl_sqlx_for_machine_id_newtype!(PredictedHostMachineId);
