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
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use carbide_secrets::credentials::{CredentialKey, CredentialReader};
use carbide_utils::HostPortPair;
use carbide_uuid::machine::MachineId;

mod bmc_mock;
mod metrics;
mod test_support;
mod tool;

pub const DEFAULT_IPMI_PORT: u16 = 623;

#[async_trait]
pub trait IPMITool: Send + Sync + 'static {
    async fn bmc_cold_reset(
        &self,
        bmc_address: SocketAddr,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report>;

    async fn restart(
        &self,
        machine_id: &MachineId,
        bmc_address: SocketAddr,
        legacy_boot: bool,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report>;
}

pub fn tool(cred_provider: Arc<dyn CredentialReader>, attempts: Option<u32>) -> Arc<dyn IPMITool> {
    Arc::new(tool::IPMIToolImpl::new(cred_provider, attempts))
}

pub fn bmc_mock(
    bmc_proxy: Arc<ArcSwap<Option<HostPortPair>>>,
    credential_reader: Arc<dyn CredentialReader>,
) -> Arc<dyn IPMITool> {
    Arc::new(bmc_mock::IPMIToolHttpImpl::new(
        bmc_proxy,
        credential_reader,
    ))
}

pub fn test_support() -> Arc<dyn IPMITool> {
    Arc::new(test_support::IPMIToolTestImpl {})
}
