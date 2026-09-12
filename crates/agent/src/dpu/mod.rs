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
use std::fmt;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use carbide_host_support::agent_config::FmdsDpuNetworkingConfig;
use ipnetwork::IpNetwork;

pub(crate) mod host_vf_link;
pub mod interface;
pub mod link;
pub mod route;

pub(crate) const ARMOS_TEST_DATA_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/docker-env");
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct DpuNetworkInterfaces {
    pub desired: Vec<IpNetwork>,
}

#[derive(PartialOrd, PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub enum Action {
    Add,
    Remove,
}

impl Display for Action {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Action::Add => write!(f, "Add"),
            Action::Remove => write!(f, "Remove"),
        }
    }
}

impl DpuNetworkInterfaces {
    pub fn new(fmds_interface_config: &FmdsDpuNetworkingConfig) -> Self {
        DpuNetworkInterfaces {
            desired: fmds_interface_config.config.addresses.clone(),
        }
    }
}
