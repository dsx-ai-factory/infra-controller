/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::rack::RackGroupId;
use clap::Parser;
use color_eyre::eyre::WrapErr;
use mac_address::MacAddress;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge;
use uuid::Uuid;

use crate::cfg::dispatch::Dispatch;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv};

#[derive(Parser, Debug, Dispatch)]
// These inherited global flags are accepted for compatibility but do not affect this command.
#[command(defer = |cmd| cmd.mut_args(|arg| match arg.get_id().as_str() {
    "extended" | "sort_by" => arg.hide(true),
    _ => arg,
}))]
pub(crate) enum Cmd {
    /// Create immutable expected membership without configuring hardware
    #[command(
        long_about = "Create immutable expected membership without configuring hardware.\n\n\
        While a declaration exists, the racks field of its rack group cannot be changed and the group \
        cannot be deleted. Site-wide expected-rack-group replace-all and erase are also blocked. \
        Delete the declarations with expected-nvlink-domain delete before these operations."
    )]
    Create(Create),
    /// Show one declaration or list declarations in UUID order
    Show(Show),
    /// Delete a declaration and release its members without changing hardware
    Delete(Delete),
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Declare a compute tray and an NVSwitch tray in an existing rack group:
    $ nico-admin-cli expected-nvlink-domain create group-01 \
    --member 00:11:22:33:44:55 --member 00:11:22:33:44:66

Give the declaration an operator-facing name:
    $ nico-admin-cli expected-nvlink-domain create group-01 --name mini-a \
    --member 00:11:22:33:44:55 --member 00:11:22:33:44:66

")]
pub(crate) struct Create {
    /// Existing external rack group ID; nonblank, at most 128 characters
    rack_group_id: RackGroupId,
    /// Expected tray BMC MAC; repeat for each member. Requires at least one compute
    /// and one switch tray in this group, with no MAC already claimed by a domain.
    /// Discovery is not required; physical NVLink connectivity is not validated.
    #[arg(long = "member", required = true)]
    members: Vec<MacAddress>,
    /// Optional nonblank name, at most 256 characters; need not be unique
    #[arg(long)]
    name: Option<String>,
}

impl Run for Create {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let domain = ctx
            .api_client
            .0
            .create_expected_nv_link_domain(forge::CreateExpectedNvLinkDomainRequest {
                rack_group_id: Some(self.rack_group_id),
                name: self.name,
                member_bmc_mac_addresses: self
                    .members
                    .into_iter()
                    .map(|mac| mac.to_string())
                    .collect(),
            })
            .await
            .wrap_err("creating expected NVLink domain")?;
        render(
            ctx,
            forge::ExpectedNvLinkDomainList {
                domains: vec![domain],
            },
        )
        .await
    }
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all declarations:
    $ nico-admin-cli expected-nvlink-domain show

List declarations in one rack group:
    $ nico-admin-cli expected-nvlink-domain show --rack-group-id group-01

Show one declaration:
    $ nico-admin-cli expected-nvlink-domain show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Show {
    /// Declaration UUID, not an observed NMX-C domain UUID; omission lists all
    id: Option<Uuid>,
    /// Filter by external rack group ID; cannot be combined with a declaration ID
    #[arg(long, conflicts_with = "id")]
    rack_group_id: Option<RackGroupId>,
}

impl Run for Show {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let mut result = forge::ExpectedNvLinkDomainList::default();
        if let Some(id) = self.id {
            result.domains.push(
                ctx.api_client
                    .0
                    .get_expected_nv_link_domain(forge::ExpectedNvLinkDomainRequest {
                        id: Some(id.into()),
                    })
                    .await
                    .wrap_err("getting expected NVLink domain")?,
            );
        } else {
            let ids = ctx
                .api_client
                .0
                .find_expected_nv_link_domain_ids(forge::ExpectedNvLinkDomainSearchFilter {
                    rack_group_id: self.rack_group_id,
                })
                .await
                .wrap_err("listing expected NVLink domain IDs")?;
            let page_size = ctx
                .api_client
                .effective_chunk_size(ctx.config.page_size.max(1))
                .await?;
            for ids in ids.ids.chunks(page_size) {
                let page = ctx
                    .api_client
                    .0
                    .find_expected_nv_link_domains_by_ids(
                        forge::ExpectedNvLinkDomainsByIdsRequest { ids: ids.to_vec() },
                    )
                    .await
                    .wrap_err("loading expected NVLink domains")?;
                result.domains.extend(page.domains);
            }
        }
        render(ctx, result).await
    }
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Delete a declaration and release its tray membership claims:
    $ nico-admin-cli expected-nvlink-domain delete 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Delete {
    /// Declaration UUID. A missing declaration returns an error. Deleting
    /// does not change hardware or observed NVLink domains.
    #[arg(verbatim_doc_comment)]
    id: Uuid,
}

impl Run for Delete {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        ctx.api_client
            .0
            .delete_expected_nv_link_domain(forge::ExpectedNvLinkDomainRequest {
                id: Some(self.id.into()),
            })
            .await
            .wrap_err("deleting expected NVLink domain")?;
        Ok(())
    }
}

async fn render(
    ctx: &mut RuntimeContext,
    domains: forge::ExpectedNvLinkDomainList,
) -> CarbideCliResult<()> {
    match ctx.config.format {
        OutputFormat::Json => {
            let value = serde_json::to_string_pretty(&domains)
                .wrap_err("serializing expected NVLink domains")?;
            async_write!(ctx.output_file, "{value}\n")
                .wrap_err("writing expected NVLink domains")?;
        }
        OutputFormat::Yaml => {
            let value =
                serde_yaml::to_string(&domains).wrap_err("serializing expected NVLink domains")?;
            async_write!(ctx.output_file, "{value}").wrap_err("writing expected NVLink domains")?;
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            let mut table = Table::new();
            table.set_titles(row![
                "Domain ID",
                "Rack Group ID",
                "Name",
                "Member BMC MACs"
            ]);
            for domain in domains.domains {
                table.add_row(row![
                    domain.id.map(|id| id.value).unwrap_or_default(),
                    domain
                        .rack_group_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    domain.name.unwrap_or_default(),
                    domain.member_bmc_mac_addresses.join(", "),
                ]);
            }
            if ctx.config.format == OutputFormat::Csv {
                async_write_table_as_csv!(ctx.output_file, table)
                    .wrap_err("writing expected NVLink domain CSV")?;
            } else {
                async_write!(ctx.output_file, "{table}")
                    .wrap_err("writing expected NVLink domain table")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
