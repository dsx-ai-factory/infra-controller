/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::SetTenantNicFirmwareConfigRequest;
use rpc::forge::set_tenant_nic_firmware_config_request::Action;

use super::super::output::write_mutation_result;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn set(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let response = ctx
        .api_client
        .0
        .set_tenant_nic_firmware_config(SetTenantNicFirmwareConfigRequest {
            tenant_organization_id: args.tenant_org_id.to_string(),
            if_version_match: args.if_version_match.to_string(),
            action: Some(Action::ProfileId(args.profile.to_string())),
        })
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to set NIC firmware profile {} for tenant {} at version {}; \
                 after a connection error, inspect firmware nic tenant show before another write",
                args.profile, args.tenant_org_id, args.if_version_match
            )
        })?;
    write_mutation_result(response.firmware_config, &args.tenant_org_id, ctx).await
}
