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

use super::super::common::{DpuTarget, ExtensionServiceType};
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Create a Kubernetes-pod extension service:
    $ nico-admin-cli extension-service create --name my-service --type kubernetes-pod \
    --data '{\"image\":\"my-registry/my-service:1.0\"}'

Create a DPF Helm chart on all DPUs used by instance networking:
    $ nico-admin-cli extension-service create --name my-helm-service --type dpf-helm-chart \
    --dpu-target all-active --data \
    '{\"repoURL\":\"oci://registry.example.com/charts\",\"chartName\":\"my-service\",\"chartVersion\":\"1.2.3\",\"security.privileged\":false}'

Create with an explicit service ID and a description:
    $ nico-admin-cli extension-service create --id 12345678-1234-5678-90ab-cdef01234567 \
    --name my-service --type kubernetes-pod --data '{\"image\":\"my-registry/my-service:1.0\"}' \
    --description \"Front-end telemetry agent\"

Create scoped to a tenant organization:
    $ nico-admin-cli extension-service create --name my-service --type kubernetes-pod \
    --data '{\"image\":\"my-registry/my-service:1.0\"}' --tenant-organization-id fds34511233a

Create with private-registry pull credentials:
    $ nico-admin-cli extension-service create --name my-service --type kubernetes-pod \
    --data '{\"image\":\"my-registry/my-service:1.0\"}' --registry-url my-registry.example.com \
    --username admin --password mypassword

")]
pub(crate) struct Args {
    #[clap(
        short = 'i',
        long = "id",
        help = "The extension service ID to create (optional)"
    )]
    service_id: Option<String>,

    #[clap(short = 'n', long = "name", help = "Extension service name")]
    service_name: String,

    #[clap(short = 't', long = "type", help = "Extension service type")]
    service_type: ExtensionServiceType,

    #[clap(
        long,
        value_enum,
        required_if_eq("service_type", "dpf-helm-chart"),
        help = "Immutable Helm placement policy: primary = primary DPU, all-active = DPUs used by instance networking, all = all attached DPUs. Required for dpf-helm-chart and unsupported for kubernetes-pod"
    )]
    dpu_target: Option<DpuTarget>,

    #[clap(long, help = "Extension service description (optional)")]
    description: Option<String>,

    #[clap(long, help = "Tenant organization ID")]
    tenant_organization_id: Option<String>,

    #[clap(short = 'd', long, help = "Extension service data")]
    data: String,

    #[clap(long, help = "Registry URL for the service credential (optional)")]
    registry_url: Option<String>,

    #[clap(long, help = "Username for the service credential (optional)")]
    username: Option<String>,

    #[clap(long, help = "Password for the service credential (optional)")]
    password: Option<String>,

    #[clap(
        long,
        help = "JSON array containing a defined set of extension observability configs (optional)"
    )]
    observability: Option<String>,
}

impl TryFrom<Args> for ::rpc::forge::CreateDpuExtensionServiceRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        if args.service_type == ExtensionServiceType::KubernetesPod && args.dpu_target.is_some() {
            return Err(CarbideCliError::GenericError(
                "--dpu-target is unsupported for kubernetes-pod".to_string(),
            ));
        }

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
                    registry_url: args.registry_url.unwrap_or_default(),
                    r#type: Some(Type::UsernamePassword(rpc::forge::UsernamePassword {
                        username: args.username.unwrap_or_default(),
                        password: args.password.unwrap_or_default(),
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
            dpu_target: args.dpu_target.map(|target| match target {
                DpuTarget::Primary => ::rpc::forge::DpuExtensionServiceDpuTarget::Primary as i32,
                DpuTarget::AllActive => {
                    ::rpc::forge::DpuExtensionServiceDpuTarget::AllActive as i32
                }
                DpuTarget::All => ::rpc::forge::DpuExtensionServiceDpuTarget::All as i32,
            }),
            service_id: args.service_id,
            service_name: args.service_name,
            service_type: args.service_type as i32,
            tenant_organization_id: args.tenant_organization_id.unwrap_or_default(),
            data: args.data,
            description: args.description,
            credential,
            observability: (!observability.is_empty()).then_some(
                ::rpc::forge::DpuExtensionServiceObservability {
                    configs: observability,
                },
            ),
            service_vpc_interfaces: vec![],
        })
    }
}
