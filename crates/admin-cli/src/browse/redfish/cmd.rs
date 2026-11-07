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

use std::collections::HashMap;

use serde::Serialize;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn browse(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let data = api_client.0.redfish_browse(args.uri).await?;

    #[derive(Serialize, Debug)]
    struct Output {
        text: serde_json::Value,
        headers: HashMap<String, String>,
    }

    // The API returns the raw response body as a string. Pretty-print it as
    // JSON when it parses; otherwise print the raw body verbatim so a non-JSON
    // response is still shown in full (this is a browse/debug tool — surfacing
    // the actual body matters more than enforcing a JSON contract).
    let text = match serde_json::from_str(&data.text) {
        Ok(text) => text,
        Err(_) => {
            println!("{}", data.text);
            return Ok(());
        }
    };

    let output = Output {
        text,
        headers: data.headers,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("Output is always serializable")
    );

    Ok(())
}
