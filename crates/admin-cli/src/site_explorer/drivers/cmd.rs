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

use std::collections::BTreeMap;
use std::net::IpAddr;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::{BmcDriverSelection, BmcEndpointRequest};
use prettytable::{Row, Table};
use serde::{Deserialize, Serialize};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv};

pub(super) async fn drivers(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    api_client: &ApiClient,
    args: Args,
) -> CarbideCliResult<()> {
    let response = api_client
        .0
        .find_bmc_driver_selection(BmcEndpointRequest {
            ip_address: args.address.to_string(),
            mac_address: None,
        })
        .await?;
    let view = DriverSelectionView::try_from(response)?;

    match output_format {
        OutputFormat::Json => {
            async_write!(output_file, "{}", serde_json::to_string_pretty(&view)?)?;
        }
        OutputFormat::Yaml => {
            async_write!(output_file, "{}", serde_yaml::to_string(&view)?)?;
        }
        OutputFormat::Csv => {
            async_write_table_as_csv!(output_file, build_table(&view))?;
        }
        _ => {
            async_write!(output_file, "{}", render(args.address, &view)?)?;
        }
    }
    Ok(())
}

/// A driver selection in the form `bmc_drivers::ResolvedSelection` serializes to.
#[derive(Debug, Deserialize, Serialize)]
struct Selection {
    drivers: BTreeMap<String, String>,
    matched_rules: Vec<MatchedRule>,
    #[serde(default)]
    quirks: Vec<String>,
    hash: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct MatchedRule {
    capability: String,
    rule_id: String,
}

#[derive(Debug, Serialize)]
struct DriverSelectionView {
    selection: Option<Selection>,
    identity: Option<serde_json::Value>,
}

impl TryFrom<BmcDriverSelection> for DriverSelectionView {
    type Error = serde_json::Error;

    fn try_from(response: BmcDriverSelection) -> Result<Self, Self::Error> {
        Ok(Self {
            selection: response
                .selection
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            identity: response
                .identity
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        })
    }
}

/// One row per capability; the rule cell is empty for the compiled default.
fn build_table(view: &DriverSelectionView) -> Box<Table> {
    let mut table = Table::new();
    table.set_titles(Row::from(vec!["Capability", "Driver", "Rule"]));
    if let Some(selection) = &view.selection {
        for (capability, driver) in &selection.drivers {
            let rule = selection
                .matched_rules
                .iter()
                .find(|matched| &matched.capability == capability)
                .map(|matched| matched.rule_id.clone())
                .unwrap_or_default();
            table.add_row(Row::from(vec![capability.clone(), driver.clone(), rule]));
        }
    }
    Box::new(table)
}

fn render(address: IpAddr, view: &DriverSelectionView) -> serde_json::Result<String> {
    let Some(selection) = &view.selection else {
        return Ok(format!(
            "No drivers are selected for {address} until site exploration records its service root\n"
        ));
    };
    let quirks = if selection.quirks.is_empty() {
        "none".to_string()
    } else {
        selection.quirks.join(", ")
    };
    Ok(format!(
        "{}Quirks: {quirks}\nRules: {}\nIdentity:\n{}\n",
        build_table(view),
        selection.hash,
        serde_json::to_string_pretty(&view.identity)?
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn renders_each_capability_with_the_rule_that_chose_it() {
        let address = IpAddr::from([192, 0, 2, 10]);
        let view = DriverSelectionView::try_from(BmcDriverSelection {
            selection: Some(
                json!({
                    "drivers": { "lockdown": "lenovo-xcc", "power": "standard" },
                    "matched_rules": [{ "capability": "lockdown", "rule_id": "lenovo-xcc" }],
                    "quirks": ["lenovo_force_restart_hangs"],
                    "hash": "ab12",
                })
                .to_string(),
            ),
            identity: Some(json!({ "service_root": { "vendor": "Lenovo" } }).to_string()),
        })
        .unwrap();

        let rendered = render(address, &view).unwrap();
        let lines: Vec<Vec<&str>> = rendered
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.split('|').map(str::trim).collect())
            .collect();
        assert_eq!(
            lines,
            vec![
                vec!["", "Capability", "Driver", "Rule", ""],
                vec!["", "lockdown", "lenovo-xcc", "lenovo-xcc", ""],
                vec!["", "power", "standard", "", ""],
            ]
        );
        assert!(rendered.contains("Quirks: lenovo_force_restart_hangs\n"));
        assert!(rendered.contains("Rules: ab12\n"));
        assert!(rendered.contains("\"vendor\": \"Lenovo\""));

        let unselected = DriverSelectionView::try_from(BmcDriverSelection::default()).unwrap();
        assert_eq!(
            render(address, &unselected).unwrap(),
            "No drivers are selected for 192.0.2.10 until site exploration records its service root\n"
        );
    }
}
