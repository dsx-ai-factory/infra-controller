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
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use carbide_utils::HostPortPair;

mod bmc_mock;
mod metrics;
mod test_support;
mod tool;

pub const DEFAULT_IPMI_PORT: u16 = 623;

/// Whether a failed single-attempt reset could have reached the BMC.
#[derive(Debug)]
pub enum RestartError {
    /// Preparation failed before a command was dispatched.
    NotSent(eyre::Report),
    /// Dispatch began; the failure does not establish whether the BMC acted.
    Uncertain(eyre::Report),
}

impl RestartError {
    fn into_inner(self) -> eyre::Report {
        match self {
            Self::NotSent(error) | Self::Uncertain(error) => error,
        }
    }
}

#[async_trait]
pub trait IPMITool: Send + Sync + 'static {
    /// Send one chassis reset attempt without retrying its result.
    /// The production tool bounds the command to 60 seconds and terminates it
    /// when the owning future is dropped. Unsupported implementations fail before sending.
    async fn restart_once(
        &self,
        _bmc_address: SocketAddr,
        _credential_key: &CredentialKey,
    ) -> Result<(), RestartError> {
        Err(RestartError::NotSent(eyre::eyre!(
            "single-attempt chassis reset is not supported by this IPMI tool"
        )))
    }

    /// Send an ordinary chassis reset using credentials prepared for this invocation.
    /// Keep the transport's configured retry policy. Never look up a replacement secret.
    async fn restart_with_credentials(
        &self,
        _bmc_address: SocketAddr,
        _credentials: &Credentials,
    ) -> Result<(), eyre::Report> {
        Err(eyre::eyre!(
            "this IPMI tool does not support a prepared chassis reset"
        ))
    }

    async fn bmc_cold_reset(
        &self,
        bmc_address: SocketAddr,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report>;

    async fn restart(
        &self,
        bmc_address: SocketAddr,
        legacy_boot: bool,
        credential_key: &CredentialKey,
    ) -> Result<(), eyre::Report>;
}

/// Creates an IPMI transport whose command Events use the supplied destination.
/// Retain its provider while transport operations can run.
pub fn tool(
    cred_provider: Arc<dyn CredentialReader>,
    attempts: Option<u32>,
    instrumentation: carbide_instrument::Instrumentation,
) -> Arc<dyn IPMITool> {
    Arc::new(tool::IPMIToolImpl::new(
        cred_provider,
        attempts,
        instrumentation,
    ))
}

/// Creates an IPMI transport whose command Events use the supplied destination.
/// Retain its provider while transport operations can run.
pub fn bmc_mock(
    bmc_proxy: Arc<ArcSwap<Option<HostPortPair>>>,
    credential_reader: Arc<dyn CredentialReader>,
    instrumentation: carbide_instrument::Instrumentation,
) -> Arc<dyn IPMITool> {
    Arc::new(bmc_mock::IPMIToolHttpImpl::new(
        bmc_proxy,
        credential_reader,
        instrumentation,
    ))
}

pub fn test_support() -> Arc<dyn IPMITool> {
    Arc::new(test_support::IPMIToolTestImpl {})
}
