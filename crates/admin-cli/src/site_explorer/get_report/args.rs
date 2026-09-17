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

use std::net::IpAddr;

use clap::{ArgGroup, Parser};

#[derive(Parser, Debug, PartialEq)]
#[command(after_long_help = "\
EXAMPLES:

Dump the entire latest report as JSON:
    $ nico-admin-cli site-explorer get-report all

Show discovered managed-host details:
    $ nico-admin-cli site-explorer get-report managed-host

Show explored endpoint details:
    $ nico-admin-cli site-explorer get-report endpoint

")]
pub(crate) enum Args {
    #[clap(about = "Get everything in Json")]
    All,
    #[clap(about = "Get discovered host details.")]
    ManagedHost(ManagedHostInfo),
    #[clap(about = "Get Endpoint details.")]
    Endpoint(EndpointInfo),
}

#[derive(Parser, Debug, PartialEq)]
#[clap(group(ArgGroup::new("selector").required(false).args(&["erroronly", "successonly"])))]
#[command(after_long_help = "\
EXAMPLES:

List all explored endpoints:
    $ nico-admin-cli site-explorer get-report endpoint

Show one endpoint by BMC IP:
    $ nico-admin-cli site-explorer get-report endpoint 192.0.2.10

Show only endpoints that errored, filtered by vendor:
    $ nico-admin-cli site-explorer get-report endpoint --erroronly --vendor nvidia

Show only endpoints not yet paired to a managed host:
    $ nico-admin-cli site-explorer get-report endpoint --unpairedonly

")]
pub(crate) struct EndpointInfo {
    #[clap(help = "BMC IP address of Endpoint.")]
    pub(crate) address: Option<IpAddr>,

    #[clap(
        short,
        long,
        help = "Filter based on vendor. Valid only for table view."
    )]
    pub(crate) vendor: Option<String>,

    #[clap(
        long,
        action,
        help = "By default shows all endpoints. If wants to see unpairedonly, choose this option."
    )]
    pub(crate) unpairedonly: bool,

    #[clap(long, action, help = "Show only endpoints which have error.")]
    pub(crate) erroronly: bool,

    #[clap(long, action, help = "Show only endpoints which have no error.")]
    pub(crate) successonly: bool,
}

#[derive(Parser, Debug, PartialEq)]
#[command(after_long_help = "\
EXAMPLES:

List all discovered managed hosts:
    $ nico-admin-cli site-explorer get-report managed-host

Show one managed host by its host/DPU BMC IP:
    $ nico-admin-cli site-explorer get-report managed-host 192.0.2.10

Filter managed hosts by vendor:
    $ nico-admin-cli site-explorer get-report managed-host --vendor nvidia

")]
pub(crate) struct ManagedHostInfo {
    #[clap(help = "BMC IP address of host or DPU")]
    pub(super) address: Option<IpAddr>,

    #[clap(
        short,
        long,
        help = "Filter based on vendor. Valid only for table view."
    )]
    pub(super) vendor: Option<String>,
}
