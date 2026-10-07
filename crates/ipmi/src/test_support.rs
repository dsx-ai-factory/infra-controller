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

use std::net::SocketAddr;

use async_trait::async_trait;
use carbide_secrets::credentials::CredentialKey;
use carbide_uuid::machine::MachineId;

use crate::IPMITool;

pub(super) struct IPMIToolTestImpl {}

#[async_trait]
impl IPMITool for IPMIToolTestImpl {
    async fn restart(
        &self,
        _machine_id: &MachineId,
        _bmc_address: SocketAddr,
        _legacy_boot: bool,
        _credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report> {
        Ok(())
    }

    async fn bmc_cold_reset(
        &self,
        _bmc_address: SocketAddr,
        _credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report> {
        Ok(())
    }
}
