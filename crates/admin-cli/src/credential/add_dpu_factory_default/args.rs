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

use clap::Parser;
use rpc::{CredentialType, forge as forgerpc};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Add the factory-default DPU BMC credential for all unrecognized models (backward-compatible default):
    $ nico-admin-cli credential add-dpu-factory-default --username root --password mypassword

Add a model-specific factory-default for BlueField-3 DPUs:
    $ nico-admin-cli credential add-dpu-factory-default --model bf3 --username root --password mypassword

Add a model-specific factory-default for BlueField-4 DPUs:
    $ nico-admin-cli credential add-dpu-factory-default --model bf4 --username admin --password mynewpassword

")]
pub(crate) struct Args {
    #[clap(long, required(true), help = "Default username: root, ADMIN, etc")]
    username: String,
    #[clap(long, required(true), help = "DPU manufacturer default password")]
    password: String,
    #[clap(
        long,
        default_value = "unknown",
        help = "DPU model: bf2, bf3, bf4, or unknown (catch-all / backward-compatible default)"
    )]
    model: bmc_vendor::DpuModel,
}

impl From<Args> for forgerpc::CredentialCreationRequest {
    fn from(args: Args) -> Self {
        Self {
            credential_type: CredentialType::DpuBmcFactoryDefault.into(),
            username: Some(args.username),
            password: args.password,
            mac_address: None,
            vendor: Some(args.model.to_string()),
            credential_name: None,
        }
    }
}
