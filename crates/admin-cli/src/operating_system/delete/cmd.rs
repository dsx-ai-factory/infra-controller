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

use ::rpc::forge::DeleteOperatingSystemRequest;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::operating_system::common::str_to_os_id;
use crate::rpc::ApiClient;

pub(super) async fn delete(opts: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let id = str_to_os_id(&opts.id)?;

    api_client
        .0
        .delete_operating_system(DeleteOperatingSystemRequest { id: Some(id) })
        .await
        .map_err(CarbideCliError::from)?;

    println!("Operating system {} deleted.", opts.id);
    Ok(())
}
