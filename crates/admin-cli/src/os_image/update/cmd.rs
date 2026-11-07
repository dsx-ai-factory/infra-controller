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

use super::args::{Args, UpdateRequest};
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn update(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let req: UpdateRequest = args.try_into()?;
    let image = api_client
        .update_os_image(
            req.id,
            req.auth_type,
            req.auth_token,
            req.name,
            req.description,
        )
        .await?;
    if let Some(x) = image.attributes {
        if let Some(y) = x.id {
            println!("OS image {y} updated successfully.");
        } else {
            eprintln!("Updating the OS image may have failed, image id missing.");
        }
    } else {
        eprintln!("Updating the OS image may have failed, image attributes missing.");
    }
    Ok(())
}
