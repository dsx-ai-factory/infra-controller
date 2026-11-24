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
use carbide_uuid::extension_service::ExtensionServiceId;
use clap::Parser;
use rpc::errors::RpcDataConversionError;
use rpc::forge::{
    DpuExtensionServiceObservability, ServiceVpcAddressFamily, ServiceVpcInterfaceRequirement,
    ServiceVpcInterfaceRequirements, UpdateDpuExtensionServiceRequest,
};

use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Update only the registration's metadata:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --description \"Updated telemetry agent\" --data ''

Revise a Ready Helm chart definition and preserve its registered service interface:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --data '{\"repoURL\":\"oci://registry.example.com/charts\",\"chartName\":\"my-service\",\"chartVersion\":\"2.0.0\",\"serviceID\":\"my-service\",\"deployInCluster\":false,\"security\":{\"privileged\":false}}'

Submit a Helm chart definition with its complete IPv4 requirement:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --service-vpc-family ipv4 --if-version-ctr-match 3 --data \
    '{\"repoURL\":\"oci://registry.example.com/charts\",\"chartName\":\"my-service\",\"chartVersion\":\"2.0.0\",\"serviceID\":\"my-service\",\"deployInCluster\":false,\"security\":{\"privileged\":false}}'

Remove the service interface requirement from a Ready registration after all attachments are gone:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --clear-service-vpc --data \
    '{\"repoURL\":\"oci://registry.example.com/charts\",\"chartName\":\"my-service\",\"chartVersion\":\"2.0.0\",\"serviceID\":\"my-service\",\"deployInCluster\":false,\"security\":{\"privileged\":false}}'

Rename a Pod service, update its description and rotate its registry credentials:
    $ nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 \
    --name my-renamed-service --description \"Updated telemetry agent\" \
    --registry-url my-registry.example.com --username admin --password mynewpassword --data \
    '{\"apiVersion\":\"v1\",\"kind\":\"Pod\",\"metadata\":{\"name\":\"my-service\"},\"spec\":{\"containers\":[{\"name\":\"service\",\"image\":\"my-registry.example.com/my-service:2.0\"}]}}'

")]
pub(crate) struct Args {
    #[clap(short = 'i', long = "id", help = "The extension service ID to update")]
    service_id: ExtensionServiceId,

    #[arg(
        long,
        value_enum,
        help = "Submit one service interface requirement for a demand-driven DPF Helm chart",
        long_help = "Submit the complete service interface requirement for a demand-driven DPF Helm chart. Pass the stored family to keep it. Changing the family requires that no active or terminating attachment references the registration. Use either this option or --clear-service-vpc. Omit both to read and resubmit the stored requirement. An accepted Helm definition update with an IPv6 requirement waits in Updating while IPv6 registration reconciliation is unavailable. Use extension-service show to check the Lifecycle Reason."
    )]
    pub(super) service_vpc_family: Option<ServiceVpcAddressFamily>,

    #[arg(
        long,
        conflicts_with = "service_vpc_family",
        help = "Remove the service interface requirement from a Ready Helm registration with no active or terminating attachments"
    )]
    pub(super) clear_service_vpc: bool,

    #[clap(
        short = 'n',
        long = "name",
        help = "New extension service name (optional)"
    )]
    service_name: Option<String>,

    #[clap(long, help = "New extension service description (optional)")]
    description: Option<String>,

    #[clap(
        short = 'd',
        long,
        help = "Complete desired service definition",
        long_help = "Complete desired service definition. For a metadata-only update, pass --data '' with --name or --description, keep the existing family, and omit credential and observability flags. Helm definition updates require Ready, keep the chart's serviceID, and revise stable V1. Core rejects a Helm definition update when the parsed chart data and family requirement are both unchanged."
    )]
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
        help = "Update only if the registration's version counter matches this revision. When resubmitting the stored family, the CLI defaults to the revision it read"
    )]
    if_version_ctr_match: Option<i32>,

    #[clap(
        long,
        help = "JSON array containing a defined set of extension observability configs (optional)"
    )]
    observability: Option<String>,
}

impl Args {
    /// Validates local fields before a caller reads the registration to preserve its requirements.
    pub(super) fn into_request(self) -> CarbideCliResult<UpdateDpuExtensionServiceRequest> {
        let args = self;
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

        let observability = args
            .observability
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .map(|configs| DpuExtensionServiceObservability { configs });

        Ok(UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(ServiceVpcInterfaceRequirements {
                interfaces: args
                    .service_vpc_family
                    .into_iter()
                    .map(|family| ServiceVpcInterfaceRequirement {
                        address_family: family.into(),
                    })
                    .collect(),
            }),
            service_id: args.service_id.to_string(),
            service_name: args.service_name,
            description: args.description,
            data: args.data,
            credential,
            if_version_ctr_match: args.if_version_ctr_match,
            observability,
        })
    }
}
