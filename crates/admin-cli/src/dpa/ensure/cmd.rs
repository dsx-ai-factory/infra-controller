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
use ::rpc::forge::{self as forgerpc};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

// ensure, similar to persist, is an RPC endpoint meant for
// debugging purposes only, and may eventually go away. The
// arguments will be used to build a DpaInterfaceCreationRequest,
// which is turned into a NewDpaInterface within carbide-api,
// and will then (in this case) be passed to ensure. Ensure
// is different than persist in that persist fails if the
// interface already exists, while ensure will create the
// interface if it doesn't exist, or return the existing
// interface otherwise.
pub(super) async fn ensure(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let interface = api_client.0.ensure_dpa_interface(args).await?;

    if output_format == OutputFormat::Json {
        println!("{}", serde_json::to_string_pretty(&interface)?);
    } else {
        println!(
            "{}",
            convert_dpa_to_nice_format(&interface).unwrap_or_else(|x| x.to_string())
        );
    }

    Ok(())
}

fn convert_dpa_to_nice_format(dpa: &forgerpc::DpaInterface) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let data = vec![
        ("ID", dpa.id.map(|id| id.to_string()).unwrap_or_default()),
        (
            "MACHINE ID",
            dpa.machine_id.map(|id| id.to_string()).unwrap_or_default(),
        ),
        ("CREATED", dpa.created.unwrap_or_default().to_string()),
        ("UPDATED", dpa.updated.unwrap_or_default().to_string()),
        (
            "DELETED",
            match dpa.deleted {
                Some(ts) => ts.to_string(),
                None => "".to_string(),
            },
        ),
        ("STATE", dpa.controller_state.to_string()),
    ];

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    writeln!(&mut lines, "STATE HISTORY: (Latest 5 only)")?;
    if dpa.history.is_empty() {
        writeln!(&mut lines, "\tEMPTY")?;
    } else {
        writeln!(
            &mut lines,
            "\tState          Version                      Time"
        )?;
        writeln!(
            &mut lines,
            "\t---------------------------------------------------"
        )?;
        for x in dpa.history.iter().rev().take(5).rev() {
            writeln!(
                &mut lines,
                "\t{:<15} {:25} {}",
                x.state,
                x.version,
                x.time.unwrap_or_default()
            )?;
        }
    }

    Ok(lines)
}
