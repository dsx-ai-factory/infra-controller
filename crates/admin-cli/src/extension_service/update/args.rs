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

use ::rpc::forge::dpu_extension_service_credential::Type;
use clap::Parser;
use rpc::errors::RpcDataConversionError;

use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Update the data of an existing service:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --data '{\"image\":\"my-registry/my-service:2.0\"}'

Rename a service and update its description:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --name my-renamed-service --description \"Updated telemetry agent\" \
    --data '{\"image\":\"my-registry/my-service:2.0\"}'

Rotate the registry pull credentials:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --data '{\"image\":\"my-registry/my-service:2.0\"}' --registry-url my-registry.example.com \
    --username admin --password mynewpassword

Update only if the version counter matches (optimistic concurrency):
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --data '{\"image\":\"my-registry/my-service:2.0\"}' --if-version-ctr-match 3

")]
pub(crate) struct Args {
    #[clap(short = 'i', long = "id", help = "The extension service ID to update")]
    service_id: String,

    #[clap(
        short = 'n',
        long = "name",
        help = "New extension service name (optional)"
    )]
    service_name: Option<String>,

    #[clap(long, help = "New extension service description (optional)")]
    description: Option<String>,

    #[clap(short = 'd', long, help = "New extension service data")]
    data: String,

    #[clap(long, help = "New registry URL for the service credential (optional)")]
    registry_url: Option<String>,

    #[clap(
        short = 'u',
        long,
        help = "New username for the service credential (optional)"
    )]
    username: Option<String>,

    #[clap(
        short = 'p',
        long,
        help = "New password for the service credential (optional)"
    )]
    password: Option<String>,

    #[clap(
        long,
        help = "Update only if current number of versions matches this number (optional)"
    )]
    if_version_ctr_match: Option<i32>,

    #[clap(
        long,
        help = "JSON array containing a defined set of extension observability configs (optional)"
    )]
    observability: Option<String>,
}

impl TryFrom<Args> for ::rpc::forge::UpdateDpuExtensionServiceRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let credential =
            if args.username.is_some() || args.password.is_some() || args.registry_url.is_some() {
                if args.username.is_none() || args.password.is_none() || args.registry_url.is_none()
                {
                    return Err(CarbideCliError::GenericError(
                    "All of username, password and registry URL are required to create credential"
                        .to_string(),
                ));
                }

                Some(::rpc::forge::DpuExtensionServiceCredential {
                    registry_url: args
                        .registry_url
                        .ok_or(RpcDataConversionError::MissingArgument("registry_url"))?,
                    r#type: Some(Type::UsernamePassword(rpc::forge::UsernamePassword {
                        username: args
                            .username
                            .ok_or(RpcDataConversionError::MissingArgument("username"))?,
                        password: args
                            .password
                            .ok_or(RpcDataConversionError::MissingArgument("password"))?,
                    })),
                })
            } else {
                None
            };

        let observability: Vec<::rpc::forge::DpuExtensionServiceObservabilityConfig> =
            if let Some(r) = args.observability {
                serde_json::from_str(&r)?
            } else {
                vec![]
            };

        Ok(Self {
            // TODO(Service VPC 03): Expose the complete service interface definition.
            // Omission lets Core accept services with no interfaces and reject incomplete updates.
            service_vpc_interfaces: None,
            service_id: args.service_id,
            service_name: args.service_name,
            description: args.description,
            data: args.data,
            credential,
            if_version_ctr_match: args.if_version_ctr_match,
            observability: Some(::rpc::forge::DpuExtensionServiceObservability {
                configs: observability,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the CLI omits a service-interface definition it cannot express,
    /// so Core can reject incomplete updates to networked services.
    #[test]
    fn update_omits_unexpressed_service_interface_definition() {
        // Parse the smallest valid update through the public command arguments.
        let args = Args::try_parse_from(["update", "--id", "service-id", "--data", "{}"])
            .expect("valid update arguments");

        // The request must not misrepresent the missing controls as an explicit removal.
        let request = ::rpc::forge::UpdateDpuExtensionServiceRequest::try_from(args)
            .expect("valid update request");
        assert!(request.service_vpc_interfaces.is_none());
    }
}
