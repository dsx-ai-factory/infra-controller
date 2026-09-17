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

use rpc::forge::ExpectedSwitch;
use rpc::forge_api_client::expected_switch_update_mask;

use super::args::Args;
use crate::rpc::ApiClient;

pub(super) async fn update(data: Args, api_client: &ApiClient) -> color_eyre::Result<()> {
    let patch: ExpectedSwitch = data.try_into()?;
    let update_mask = expected_switch_update_mask(&patch);

    api_client
        .0
        .update_expected_switch_with_mask(patch, &update_mask)
        .await?;

    Ok(())
}
