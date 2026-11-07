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

use ::rpc::forge::{self as rpc, InsertPowerShelfHealthReportRequest};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::health_utils;
use crate::rpc::ApiClient;

pub(super) async fn add(api_client: &ApiClient, args: Args) -> CarbideCliResult<()> {
    let report =
        health_utils::resolve_health_report(args.template, args.health_report, args.message)?;

    if args.print_only {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let request = InsertPowerShelfHealthReportRequest {
        power_shelf_id: Some(args.power_shelf_id),
        health_report_entry: Some(rpc::HealthReportEntry {
            report: Some(report.into()),
            mode: if args.replace {
                rpc::HealthReportApplyMode::Replace
            } else {
                rpc::HealthReportApplyMode::Merge
            } as i32,
        }),
    };
    api_client
        .0
        .insert_power_shelf_health_report(request)
        .await?;

    Ok(())
}
