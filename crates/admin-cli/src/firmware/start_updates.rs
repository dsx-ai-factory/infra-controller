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

use ::rpc::forge as forgerpc;
use chrono::TimeZone;

use crate::errors::CarbideCliError;
use crate::managed_host::StartUpdates;
use crate::rpc::ApiClient;

pub(crate) async fn start_updates(
    api_client: &ApiClient,
    options: StartUpdates,
) -> color_eyre::Result<()> {
    let (start_timestamp, end_timestamp) = if options.cancel {
        (None, None)
    } else {
        let start = if let Some(start) = options.start {
            if let Some(start) = time_parse(start.as_str()) {
                start
            } else {
                return Err(CarbideCliError::GenericError(
                    "Invalid time format for --start".to_string(),
                )
                .into());
            }
        } else {
            chrono::Utc::now()
        };
        let end = if let Some(end) = options.end {
            if let Some(end) = time_parse(end.as_str()) {
                end
            } else {
                return Err(CarbideCliError::GenericError(
                    "Invalid time format for --end".to_string(),
                )
                .into());
            }
        } else {
            start
                .checked_add_signed(chrono::TimeDelta::days(1))
                .ok_or_else(|| eyre::eyre!("invalid date: {start}"))?
        };
        (Some(start), Some(end))
    };
    let request = forgerpc::SetFirmwareUpdateTimeWindowRequest {
        machine_ids: options.machines,
        start_timestamp: start_timestamp.map(Into::into),
        end_timestamp: end_timestamp.map(Into::into),
    };
    api_client
        .0
        .set_firmware_update_time_window(request)
        .await?;
    println!("Request complete");
    Ok(())
}

fn time_parse(input: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(output) = chrono::DateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%z") {
        Some(output.with_timezone(&chrono::Utc))
    } else if let Ok(output) = chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S") {
        chrono::Local
            .from_local_datetime(&output)
            .earliest()
            .map(|x| x.to_utc())
    } else {
        None
    }
}
