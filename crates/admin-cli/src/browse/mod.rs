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

//! `browse` groups the API-server-proxied "walk a subsystem's resource tree"
//! operations. Unlike `redfish` (which connects directly to a BMC and so
//! requires `--address`), every `browse` subcommand goes through the API
//! client and needs no BMC connection details: `redfish` (by URI), `ufm` (by
//! fabric + path), and `nmxc` (by chassis + operation).

mod nmxc;
mod redfish;
mod ufm;

use std::collections::HashMap;

use clap::Parser;
use serde::Serialize;

use crate::cfg::dispatch::Dispatch;
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Browse a Redfish resource tree via the API server")]
    Redfish(redfish::Args),
    #[clap(about = "Browse a UFM fabric via the API server")]
    Ufm(ufm::Args),
    #[clap(about = "Run an NMX-C browse operation via the API server")]
    Nmxc(nmxc::Args),
}

// Pretty-prints an HTTP-style browse response. The ufm/nmxm/nmxc browse RPCs
// all return the same shape — a raw body string, an HTTP status code, and the
// response headers — so they share this printer. (redfish_browse predates
// these and returns a slightly different shape, so it has its own printer.)
//
// The body is rendered as pretty JSON when it parses; otherwise the status,
// headers, and raw body are printed verbatim so a non-JSON body is shown
// rather than swallowed.
pub(crate) fn print_http_response(
    body: String,
    code: i32,
    headers: HashMap<String, String>,
) -> CarbideCliResult<()> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) else {
        println!("HTTP {code}");
        for (name, value) in &headers {
            println!("{name}: {value}");
        }
        println!("{body}");
        return Ok(());
    };

    #[derive(Serialize)]
    struct Output {
        code: i32,
        body: serde_json::Value,
        headers: HashMap<String, String>,
    }

    let output = Output {
        code,
        body: parsed,
        headers,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("Output is always serializable")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Cmd;
    use crate::test_support::{parse_leaf, raw_value};

    // `browse redfish` parses with a --uri and, unlike the `redfish`
    // subcommands, requires no BMC --address.
    #[test]
    fn parse_browse_redfish() {
        let matches =
            parse_leaf::<Cmd>(&["browse", "redfish", "--uri", "/redfish/v1"], &["redfish"])
                .expect("should parse browse redfish");
        assert_eq!(raw_value(&matches, "uri").as_deref(), Some("/redfish/v1"));
    }
}
