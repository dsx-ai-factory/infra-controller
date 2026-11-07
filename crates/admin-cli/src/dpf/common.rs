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

use carbide_uuid::machine::HostMachineId;
use clap::Parser;

use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug)]
pub(super) struct DpfQuery {
    #[clap(help = "Host machine id")]
    pub(super) host: Option<HostMachineId>,
}

impl TryFrom<&DpfQuery> for HostMachineId {
    type Error = CarbideCliError;

    fn try_from(query: &DpfQuery) -> CarbideCliResult<Self> {
        let Some(host) = query.host else {
            return Err(CarbideCliError::GenericError(
                "Host id is required!!".to_string(),
            ));
        };

        Ok(host)
    }
}
