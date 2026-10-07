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

use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{
    Metadata, SitePrefix, SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixRoutingScope,
};
use serde::Serialize;

use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv, async_writeln};

#[derive(Debug, Serialize)]
pub(super) struct SitePrefixView {
    id: Option<String>,
    prefix: Option<String>,
    tenant_organization_id: Option<String>,
    authority: Option<String>,
    routing_scope: Option<String>,
    lifecycle_state: Option<String>,
    version: String,
    quota: Option<QuotaView>,
    metadata: Option<Metadata>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct QuotaView {
    used: u32,
    limit: u32,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum SitePrefixOutput {
    One(SitePrefixView),
    Many(Vec<SitePrefixView>),
}

impl SitePrefixOutput {
    pub(super) fn one(site_prefix: SitePrefix) -> Self {
        Self::One(site_prefix.into())
    }

    pub(super) fn many(site_prefixes: Vec<SitePrefix>) -> Self {
        Self::Many(site_prefixes.into_iter().map(Into::into).collect())
    }

    fn as_slice(&self) -> &[SitePrefixView] {
        match self {
            Self::One(site_prefix) => std::slice::from_ref(site_prefix),
            Self::Many(site_prefixes) => site_prefixes.as_slice(),
        }
    }

    fn table(&self) -> Table {
        let mut table = Table::new();
        table.set_titles(Row::new(
            [
                "SitePrefixId",
                "Tenant Organization ID",
                "Authority",
                "Prefix",
                "Routing Scope",
                "Lifecycle State",
                "Version",
                "Quota",
                "Name",
            ]
            .into_iter()
            .map(Cell::new)
            .collect(),
        ));

        for site_prefix in self.as_slice() {
            table.add_row(Row::new(
                site_prefix
                    .row_values()
                    .iter()
                    .map(|value| Cell::new(value))
                    .collect(),
            ));
        }

        table
    }

    pub(super) async fn write(
        &self,
        format: &OutputFormat,
        output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> CarbideCliResult<()> {
        match format {
            OutputFormat::Json => {
                async_writeln!(output_file, "{}", serde_json::to_string_pretty(self)?)?
            }
            OutputFormat::Yaml => async_write!(output_file, "{}", serde_yaml::to_string(self)?)?,
            OutputFormat::AsciiTable => async_write!(output_file, "{}", self.table())?,
            OutputFormat::Csv => async_write_table_as_csv!(output_file, self.table())?,
        }

        Ok(())
    }
}

impl SitePrefixView {
    fn quota_display(&self) -> String {
        self.quota
            .as_ref()
            .map(|quota| format!("{}/{}", quota.used, quota.limit))
            .unwrap_or_else(|| "NA".to_string())
    }

    fn row_values(&self) -> Vec<String> {
        let name = self
            .metadata
            .as_ref()
            .map(|metadata| metadata.name.clone())
            .unwrap_or_default();

        vec![
            self.id.clone().unwrap_or_default(),
            self.tenant_organization_id.clone().unwrap_or_default(),
            self.authority.clone().unwrap_or_else(|| "NA".to_string()),
            self.prefix.clone().unwrap_or_default(),
            self.routing_scope
                .clone()
                .unwrap_or_else(|| "NA".to_string()),
            self.lifecycle_state
                .clone()
                .unwrap_or_else(|| "NA".to_string()),
            self.version.clone(),
            self.quota_display(),
            name,
        ]
    }
}

impl From<SitePrefix> for SitePrefixView {
    fn from(site_prefix: SitePrefix) -> Self {
        let (prefix, tenant_organization_id, routing_scope) = match site_prefix.config {
            Some(config) => (
                Some(config.prefix),
                config.tenant_organization_id,
                Some(routing_scope_name(config.routing_scope)),
            ),
            None => (None, None, None),
        };
        let (authority, lifecycle_state, quota) = match site_prefix.status {
            Some(status) => (
                Some(authority_name(status.authority)),
                Some(lifecycle_state_name(status.lifecycle_state)),
                status.quota.map(|quota| QuotaView {
                    used: quota.used,
                    limit: quota.limit,
                }),
            ),
            None => (None, None, None),
        };

        Self {
            id: site_prefix.id.map(|id| id.to_string()),
            prefix,
            tenant_organization_id,
            authority,
            routing_scope,
            lifecycle_state,
            version: site_prefix.version,
            quota,
            metadata: site_prefix.metadata,
            created_at: site_prefix.created_at.map(|time| time.to_string()),
            updated_at: site_prefix.updated_at.map(|time| time.to_string()),
        }
    }
}

fn authority_name(value: i32) -> String {
    match SitePrefixAuthority::try_from(value) {
        Ok(SitePrefixAuthority::OperatorManaged) => "operator-managed".to_string(),
        Ok(SitePrefixAuthority::TenantManaged) => "tenant-managed".to_string(),
        Ok(SitePrefixAuthority::Unspecified) => "unspecified".to_string(),
        Err(_) => format!("unknown ({value})"),
    }
}

fn routing_scope_name(value: i32) -> String {
    match SitePrefixRoutingScope::try_from(value) {
        Ok(SitePrefixRoutingScope::DatacenterOnly) => "datacenter-only".to_string(),
        Ok(SitePrefixRoutingScope::Unspecified) => "unspecified".to_string(),
        Err(_) => format!("unknown ({value})"),
    }
}

fn lifecycle_state_name(value: i32) -> String {
    match SitePrefixLifecycleState::try_from(value) {
        Ok(SitePrefixLifecycleState::Provisioning) => "provisioning".to_string(),
        Ok(SitePrefixLifecycleState::Ready) => "ready".to_string(),
        Ok(SitePrefixLifecycleState::Deleting) => "deleting".to_string(),
        Ok(SitePrefixLifecycleState::Error) => "error".to_string(),
        Ok(SitePrefixLifecycleState::Unspecified) => "unspecified".to_string(),
        Err(_) => format!("unknown ({value})"),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use carbide_uuid::site_prefix::SitePrefixId;
    use rpc::forge::{SitePrefixConfig, SitePrefixQuotaUsage, SitePrefixStatus};

    use super::*;

    #[test]
    fn authority_names_are_operator_facing() {
        value_scenarios!(authority_name:
            "known authorities" {
                SitePrefixAuthority::OperatorManaged as i32 => "operator-managed".to_string(),
                SitePrefixAuthority::TenantManaged as i32 => "tenant-managed".to_string(),
                SitePrefixAuthority::Unspecified as i32 => "unspecified".to_string(),
            }

            "unknown authority" {
                99 => "unknown (99)".to_string(),
            }
        );
    }

    #[test]
    fn routing_scope_names_are_operator_facing() {
        value_scenarios!(routing_scope_name:
            "known routing scopes" {
                SitePrefixRoutingScope::DatacenterOnly as i32 => "datacenter-only".to_string(),
                SitePrefixRoutingScope::Unspecified as i32 => "unspecified".to_string(),
            }

            "unknown routing scope" {
                99 => "unknown (99)".to_string(),
            }
        );
    }

    #[test]
    fn lifecycle_state_names_are_operator_facing() {
        value_scenarios!(lifecycle_state_name:
            "known lifecycle states" {
                SitePrefixLifecycleState::Provisioning as i32 => "provisioning".to_string(),
                SitePrefixLifecycleState::Ready as i32 => "ready".to_string(),
                SitePrefixLifecycleState::Deleting as i32 => "deleting".to_string(),
                SitePrefixLifecycleState::Error as i32 => "error".to_string(),
                SitePrefixLifecycleState::Unspecified as i32 => "unspecified".to_string(),
            }

            "unknown lifecycle state" {
                99 => "unknown (99)".to_string(),
            }
        );
    }

    struct OutputCase {
        authority: SitePrefixAuthority,
        tenant_organization_id: Option<&'static str>,
        quota: Option<(u32, u32)>,
    }

    type OutputSummary = (Option<String>, Option<String>, Option<(u32, u32)>, String);

    fn output_summary(
        OutputCase {
            authority,
            tenant_organization_id,
            quota,
        }: OutputCase,
    ) -> OutputSummary {
        let site_prefix = SitePrefix {
            id: Some(SitePrefixId::new()),
            config: Some(SitePrefixConfig {
                prefix: "10.0.0.0/16".to_string(),
                tenant_organization_id: tenant_organization_id.map(str::to_string),
                routing_scope: SitePrefixRoutingScope::DatacenterOnly as i32,
            }),
            status: Some(SitePrefixStatus {
                authority: authority as i32,
                lifecycle_state: SitePrefixLifecycleState::Ready as i32,
                quota: quota.map(|(used, limit)| SitePrefixQuotaUsage { used, limit }),
            }),
            metadata: Some(Metadata::default()),
            version: "V1-T0".to_string(),
            created_at: None,
            updated_at: None,
        };
        let view = SitePrefixView::from(site_prefix);
        let table_quota = view.quota_display();
        let quota = view.quota.as_ref().map(|quota| (quota.used, quota.limit));

        (
            view.authority,
            view.tenant_organization_id,
            quota,
            table_quota,
        )
    }

    #[test]
    fn structured_and_table_output_distinguish_authority_and_quota() {
        value_scenarios!(output_summary:
            "operator-managed resource" {
                OutputCase {
                    authority: SitePrefixAuthority::OperatorManaged,
                    tenant_organization_id: None,
                    quota: None,
                } => (
                    Some("operator-managed".to_string()),
                    None,
                    None,
                    "NA".to_string(),
                ),
            }

            "tenant-managed resource" {
                OutputCase {
                    authority: SitePrefixAuthority::TenantManaged,
                    tenant_organization_id: Some("fds34511233a"),
                    quota: Some((3, 8)),
                } => (
                    Some("tenant-managed".to_string()),
                    Some("fds34511233a".to_string()),
                    Some((3, 8)),
                    "3/8".to_string(),
                ),
            }
        );
    }
}
