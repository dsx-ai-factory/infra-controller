/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::instance::InstanceId;
use clap::{ArgGroup, Parser};
use eyre::WrapErr;
use rpc::forge::{InstanceDpuExtensionServiceConfig, InstanceDpuExtensionServicesConfig};

use super::parse_extension_service;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug)]
#[command(
    group(ArgGroup::new("replacement").required(true).args(["extension_services", "detach_all"])),
    override_usage = "nico-admin-cli --cloud-unsafe-op <USERNAME> instance update-extension-services [OPTIONS] --instance <INSTANCE> <--extension-service <SERVICE_JSON>|--detach-all>",
    long_about = "Replace the entire active extension-service list. Changing the list requires an instance in Ready with no deletion request. The CLI submits the full instance config with the version it read. Core rejects the update if that version changed. Put --cloud-unsafe-op <USERNAME> before instance. Supply one or more --extension-service selections or use --detach-all. Repeat --extension-service for each service and version to keep, with one selection per service ID. Omitted services begin asynchronous termination. A service/version pair gets a new attachment ID when no matching record is retained. Reactivating a retained pair without stored service interfaces reuses its record. To change the VPC for the same service and version, detach it, wait for service-interface cleanup, then attach it with the new VPC. New service VPC attachments require reconciliation for the complete service VPC lifecycle and family enablement on every target DPU's deployment. Core rejects new service VPC attachments for either family with FAILED_PRECONDITION while required reconciliation is unavailable. For an active service VPC attachment, omit service_vpc_ids or supply an empty array to keep the stored selection. You can also supply the same VPC ID from instance show.",
    after_long_help = "\
EXAMPLES:

Replace the active list with one Helm extension service without a service VPC requirement:
    $ nico-admin-cli --cloud-unsafe-op service-vpc-test instance update-extension-services \
    --instance 23072a44-0eb6-4867-9c2d-c65130b7134c \
    --extension-service '{\"service_id\":\"12345678-1234-5678-90ab-cdef01234567\",\"version\":\"V1-T1720000000000000\"}'

Replace the active list with one existing service VPC attachment:
    $ nico-admin-cli --cloud-unsafe-op service-vpc-test instance update-extension-services \
    --instance 23072a44-0eb6-4867-9c2d-c65130b7134c \
    --extension-service '{\"service_id\":\"12345678-1234-5678-90ab-cdef01234567\",\"version\":\"V1-T1720000000000000\",\"service_vpc_ids\":[\"abcdef01-2345-6789-abcd-ef0123456789\"]}'

Detach all extension services and start their asynchronous cleanup:
    $ nico-admin-cli --cloud-unsafe-op service-vpc-test instance update-extension-services \
    --instance 23072a44-0eb6-4867-9c2d-c65130b7134c --detach-all

"
)]
pub(crate) struct Args {
    #[arg(short, long, help = "Instance ID")]
    instance: InstanceId,

    #[arg(
        long = "extension-service",
        value_name = "SERVICE_JSON",
        value_parser = parse_extension_service,
        help = "Attachment to keep or add as JSON. Repeat for different services",
        long_help = "Pass a JSON object with service_id, version, and optional service_vpc_ids. Repeat with a different service ID for each selection. Use the exact version from extension-service show (stable V1 for DPF Helm). Supply one VPC UUID in service_vpc_ids when adding an attachment to a service with a registered interface. For services without a registered interface, omit service_vpc_ids or supply an empty array. For an active attachment, either form preserves the stored selection. You can also supply the same VPC ID."
    )]
    pub(super) extension_services: Vec<InstanceDpuExtensionServiceConfig>,

    #[arg(
        long,
        help = "Replace the list with an empty list and detach all services"
    )]
    detach_all: bool,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let modified_by = ctx
            .assert_cloud_unsafe_op_message()
            .wrap_err("get the required --cloud-unsafe-op value")?;
        // Clap requires a replacement mode, so an empty vector is an explicit detach.
        let service_configs = self.extension_services;
        let instance = ctx
            .api_client
            .update_instance_config_with(
                self.instance,
                |config| {
                    config.dpu_extension_services =
                        Some(InstanceDpuExtensionServicesConfig { service_configs });
                },
                |_| {},
                Some(modified_by.to_string()),
            )
            .await
            .wrap_err_with(|| {
                format!("replace extension services for instance {}", self.instance)
            })?;
        tracing::info!(
            instance_id = %self.instance,
            config_version = %instance.config_version,
            detach_all = self.detach_all,
            "Updated desired extension services"
        );
        Ok(())
    }
}
