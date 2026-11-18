/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::GetTenantNicFirmwareConfigRequest;

use super::super::output::write_config;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn show(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let response = ctx
        .api_client
        .0
        .get_tenant_nic_firmware_config(GetTenantNicFirmwareConfigRequest {
            tenant_organization_id: args.tenant_org_id.to_string(),
        })
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to read NIC firmware configuration for tenant {}",
                args.tenant_org_id
            )
        })?;
    write_config(response.firmware_config, &args.tenant_org_id, ctx).await
}
