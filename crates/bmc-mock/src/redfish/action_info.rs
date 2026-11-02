// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Typed ActionInfo descriptions for configured reset actions.

use super::schema::action_info::{ActionInfo, ParameterTypes, Parameters};
use crate::ResourceResetType;

pub(super) fn reset(odata_id: String, allowed: &[ResourceResetType]) -> ActionInfo {
    let values = allowed
        .iter()
        .map(|reset| {
            serde_json::to_value(reset)
                .expect("ResetType serializes")
                .as_str()
                .expect("ResetType has a string wire representation")
                .to_owned()
        })
        .collect::<Vec<_>>();
    ActionInfo::new(odata_id, "ResetActionInfo", "Reset Action Info").parameters(vec![
        Parameters::new("ResetType")
            .required(true)
            .data_type(ParameterTypes::String)
            .allowable_values(values),
    ])
}
