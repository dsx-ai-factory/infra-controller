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

use serde::Serialize;

use crate::{BinaryState, HeartbeatMetadata, RackPointMetadata};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceId {
    RackTrayLeak { rack_id: String },
    LiquidIsolationRequest { rack_id: String },
    ElectricalIsolationRequest { rack_id: String },
    HeartbeatTimestamp,
}

impl SourceId {
    pub fn from_rack_metadata(metadata: &RackPointMetadata) -> Self {
        match metadata {
            RackPointMetadata::LiquidIsolationRequest { rack_id, .. } => {
                Self::LiquidIsolationRequest {
                    rack_id: rack_id.clone(),
                }
            }
            RackPointMetadata::ElectricalIsolationRequest { rack_id, .. } => {
                Self::ElectricalIsolationRequest {
                    rack_id: rack_id.clone(),
                }
            }
            RackPointMetadata::RackTrayLeak { rack_id, .. } => Self::RackTrayLeak {
                rack_id: rack_id.clone(),
            },
        }
    }

    pub const fn from_heartbeat_metadata(_: &HeartbeatMetadata) -> Self {
        Self::HeartbeatTimestamp
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceValue {
    Binary(BinaryState),
    HeartbeatTimestamp(i64),
}

impl Serialize for SourceValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Binary(value) => value.serialize(serializer),
            Self::HeartbeatTimestamp(value) => serializer.serialize_i64(*value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceUpdate {
    RackTrayLeak {
        rack_id: String,
        exists: BinaryState,
    },
    LiquidIsolationRequest {
        rack_id: String,
        requested: BinaryState,
    },
    ElectricalIsolationRequest {
        rack_id: String,
        requested: BinaryState,
    },
}

impl SourceUpdate {
    pub fn liquid_isolation_request(rack_id: impl Into<String>, requested: bool) -> Self {
        Self::LiquidIsolationRequest {
            rack_id: rack_id.into(),
            requested: requested.into(),
        }
    }

    pub fn electrical_isolation_request(rack_id: impl Into<String>, requested: bool) -> Self {
        Self::ElectricalIsolationRequest {
            rack_id: rack_id.into(),
            requested: requested.into(),
        }
    }

    pub fn source_id(&self) -> SourceId {
        match self {
            Self::RackTrayLeak { rack_id, .. } => SourceId::RackTrayLeak {
                rack_id: rack_id.clone(),
            },
            Self::LiquidIsolationRequest { rack_id, .. } => SourceId::LiquidIsolationRequest {
                rack_id: rack_id.clone(),
            },
            Self::ElectricalIsolationRequest { rack_id, .. } => {
                SourceId::ElectricalIsolationRequest {
                    rack_id: rack_id.clone(),
                }
            }
        }
    }

    pub const fn value(&self) -> SourceValue {
        match self {
            Self::RackTrayLeak { exists, .. } => SourceValue::Binary(*exists),
            Self::LiquidIsolationRequest { requested, .. } => SourceValue::Binary(*requested),
            Self::ElectricalIsolationRequest { requested, .. } => SourceValue::Binary(*requested),
        }
    }
}
