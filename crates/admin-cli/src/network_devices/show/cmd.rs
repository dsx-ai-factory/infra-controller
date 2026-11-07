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

use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn handle_show(
    output_format: OutputFormat,
    query: Args,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let devices = api_client.0.get_network_topology(query).await?;

    match output_format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&devices)?),
        OutputFormat::AsciiTable => show_network_devices_info(devices)?,
        OutputFormat::Csv => println!("CSV not yet supported."),
        OutputFormat::Yaml => println!("YAML not yet supported."),
    }

    Ok(())
}

fn show_network_devices_info(data: rpc::forge::NetworkTopologyData) -> CarbideCliResult<()> {
    let mut lines = String::new();

    writeln!(&mut lines, "{}", "-".repeat(95))?;
    for network_device in data.network_devices {
        writeln!(
            &mut lines,
            "Network Device: {}/{}",
            network_device.name, network_device.id
        )?;
        writeln!(
            &mut lines,
            "Description:    {}",
            network_device.description.unwrap_or_default()
        )?;
        writeln!(
            &mut lines,
            "Mgmt IP:        {}",
            network_device.mgmt_ip.join(",")
        )?;
        writeln!(
            &mut lines,
            "Discovered Via: {}",
            network_device.discovered_via
        )?;
        writeln!(&mut lines, "Device Type:    {}", network_device.device_type)?;
        writeln!(&mut lines)?;
        writeln!(&mut lines, "Connected DPU(s):")?;
        for device in &network_device.devices {
            writeln!(
                &mut lines,
                "\t\t{} | {:8} | {}",
                device.id.unwrap_or_default(),
                device.local_port,
                device
                    .remote_port
                    .split('=')
                    .next_back()
                    .unwrap_or_default()
            )?;
        }
        writeln!(&mut lines, "{}", "-".repeat(95))?;
    }
    writeln!(&mut lines)?;

    println!("{lines}");

    Ok(())
}
