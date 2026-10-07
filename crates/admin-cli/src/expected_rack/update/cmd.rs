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

use rpc::forge::ExpectedRack;

use super::args::Args;
use crate::rpc::ApiClient;

/// update updates an existing expected rack's rack_profile_id and metadata.
pub(super) async fn update(data: Args, api_client: &ApiClient) -> color_eyre::Result<()> {
    let req: ExpectedRack = data.try_into()?;
    api_client.0.update_expected_rack(req).await?;
    Ok(())
}
