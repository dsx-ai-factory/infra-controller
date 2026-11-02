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

use model::expected_rack::ExpectedRack;
use model::metadata::Metadata;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<ExpectedRack> for rpc::forge::ExpectedRack {
    fn from(expected_rack: ExpectedRack) -> Self {
        rpc::forge::ExpectedRack {
            rack_id: Some(expected_rack.rack_id),
            rack_profile_id: Some(expected_rack.rack_profile_id),
            rack_group_id: expected_rack.rack_group_id,
            metadata: Some(expected_rack.metadata.into()),
        }
    }
}

impl TryFrom<rpc::forge::ExpectedRack> for ExpectedRack {
    type Error = RpcDataConversionError;

    fn try_from(rpc: rpc::forge::ExpectedRack) -> Result<Self, Self::Error> {
        let rack_id = rpc
            .rack_id
            .ok_or(RpcDataConversionError::MissingArgument("rack_id"))?;
        let rack_profile_id = rpc
            .rack_profile_id
            .ok_or(RpcDataConversionError::MissingArgument("rack_profile_id"))?;
        if rack_profile_id.as_str().is_empty() {
            return Err(RpcDataConversionError::InvalidArgument(
                "rack_profile_id is required".to_string(),
            ));
        }
        let metadata = Metadata::try_from(rpc.metadata.unwrap_or_default())?;

        Ok(ExpectedRack {
            rack_id,
            rack_profile_id,
            rack_group_id: rpc.rack_group_id,
            metadata,
        })
    }
}
