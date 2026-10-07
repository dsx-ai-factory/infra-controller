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

use carbide_uuid::machine::MachineType;

use crate::dpf::snapshot::args::SnapshotQuery;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn snapshot(
    query: &SnapshotQuery,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    if query.host_machine_id.machine_type() == MachineType::Dpu {
        return Err(CarbideCliError::GenericError(
            "Only host machine id is expected".to_string(),
        ));
    }

    let payload = api_client
        .get_dpf_host_snapshot(query.host_machine_id)
        .await?;
    println!("{payload}");
    Ok(())
}
