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

use ::rpc::admin_cli::output::OutputFormat;
use ::rpc::forge;
use eyre::WrapErr;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

trait VpcCreateClient {
    async fn version(&self) -> CarbideCliResult<forge::BuildInfo>;
    async fn create_vpc(&self, request: forge::VpcCreationRequest) -> CarbideCliResult<forge::Vpc>;
}

impl VpcCreateClient for ApiClient {
    async fn version(&self) -> CarbideCliResult<forge::BuildInfo> {
        Ok(self.0.version(false).await?)
    }

    async fn create_vpc(&self, request: forge::VpcCreationRequest) -> CarbideCliResult<forge::Vpc> {
        Ok(self.0.create_vpc(request).await?)
    }
}

pub(super) async fn create(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let request: forge::VpcCreationRequest = args.into();
    let vpc = create_vpc(request, api_client).await?;

    match output_format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&vpc)?),
        OutputFormat::AsciiTable => {
            println!(
                "{}",
                crate::vpc::show::cmd::convert_vpc_to_nice_format(&vpc)?
            );
        }
        _ => println!("{}", serde_yaml::to_string(&vpc)?),
    }

    Ok(())
}

async fn create_vpc(
    request: forge::VpcCreationRequest,
    api_client: &impl VpcCreateClient,
) -> CarbideCliResult<forge::Vpc> {
    // Older Core versions ignore the unknown protobuf field. Check the
    // advertised capability before creation so `true` cannot silently create
    // a VPC with SLAAC disabled.
    if request.slaac_enabled == Some(true) {
        let build_info = api_client.version().await.wrap_err(
            "while attempting to query core capabilities before creating a VPC with SLAAC enabled",
        )?;
        if !build_info
            .capabilities
            .contains(&(forge::BuildCapability::VpcSlaac as i32))
        {
            return Err(CarbideCliError::UnsupportedOperation(
                "the connected Core does not support VPC SLAAC; upgrade Core or omit --slaac-enabled true",
            ));
        }
    }

    api_client.create_vpc(request).await
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Call {
        Version,
        Create(Option<bool>),
    }

    struct FakeVpcCreateClient {
        capabilities: Vec<i32>,
        version_fails: bool,
        calls: RefCell<Vec<Call>>,
    }

    impl VpcCreateClient for FakeVpcCreateClient {
        async fn version(&self) -> CarbideCliResult<forge::BuildInfo> {
            self.calls.borrow_mut().push(Call::Version);
            if self.version_fails {
                return Err(tonic::Status::unavailable("version unavailable").into());
            }
            Ok(forge::BuildInfo {
                capabilities: self.capabilities.clone(),
                ..Default::default()
            })
        }

        async fn create_vpc(
            &self,
            request: forge::VpcCreationRequest,
        ) -> CarbideCliResult<forge::Vpc> {
            self.calls
                .borrow_mut()
                .push(Call::Create(request.slaac_enabled));
            Ok(forge::Vpc::default())
        }
    }

    async fn check_create(
        slaac_enabled: Option<bool>,
        capabilities: &[i32],
        succeeds: bool,
        expected_calls: &[Call],
    ) {
        let client = FakeVpcCreateClient {
            capabilities: capabilities.to_vec(),
            version_fails: false,
            calls: RefCell::new(Vec::new()),
        };
        let result = create_vpc(
            forge::VpcCreationRequest {
                slaac_enabled,
                ..Default::default()
            },
            &client,
        )
        .await;

        assert_eq!(result.is_ok(), succeeds);
        assert_eq!(client.calls.into_inner(), expected_calls);
    }

    #[tokio::test]
    async fn slaac_capability_check_controls_creation() {
        check_create(
            Some(true),
            &[forge::BuildCapability::VpcSlaac as i32],
            true,
            &[Call::Version, Call::Create(Some(true))],
        )
        .await;
        check_create(Some(true), &[i32::MAX], false, &[Call::Version]).await;
        check_create(Some(false), &[], true, &[Call::Create(Some(false))]).await;
        check_create(None, &[], true, &[Call::Create(None)]).await;

        let client = FakeVpcCreateClient {
            capabilities: Vec::new(),
            version_fails: true,
            calls: RefCell::new(Vec::new()),
        };
        let error = create_vpc(
            forge::VpcCreationRequest {
                slaac_enabled: Some(true),
                ..Default::default()
            },
            &client,
        )
        .await
        .expect_err("a failed capability query must stop creation");
        assert!(error.to_string().contains(
            "while attempting to query core capabilities before creating a VPC with SLAAC enabled"
        ));
        assert_eq!(client.calls.into_inner(), [Call::Version]);
    }
}
