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

use std::io::{self, Read};

use clap::Parser;
use eyre::WrapErr;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

In Bash, store the site-wide Ubuntu login password without command arguments:
    $ IFS= read -r -s -p 'DPU OS password: ' dpu_password; printf '\\n'
    $ printf '%s' \"$dpu_password\" | nico-admin-cli credential add-dpu-os --password-stdin
    $ unset dpu_password

Restart NICo API to apply the credential during DPF initialization. This can
reprovision existing DPUs. The credential can only be set once.
The password must contain 1-511 UTF-8 bytes without NUL, CR, or LF.
Remove all literal mentions of ubuntu_PASSWORD from extra_bfcfg_parameters first.

")]
pub(crate) struct Args {
    #[arg(
        long,
        required = true,
        help = "Read the password from standard input; password arguments are not accepted"
    )]
    password_stdin: bool,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let mut password = String::new();
        io::stdin()
            .take(512)
            .read_to_string(&mut password)
            .wrap_err("read the DPU OS password from standard input")?;
        ctx.api_client
            .0
            .create_credential(rpc::forge::CredentialCreationRequest {
                credential_type: rpc::forge::CredentialType::DpuOs.into(),
                username: None,
                password,
                ..Default::default()
            })
            .await
            .wrap_err("store the site-wide DPU OS credential")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn requires_stdin_and_rejects_password_arguments() {
        value_scenarios!(
            run = |args: &[&str]| Args::try_parse_from(args).is_ok();
            "stdin required" {
                &["add-dpu-os"][..] => false,
                &["add-dpu-os", "--password-stdin"][..] => true,
            }
            "password arguments rejected" {
                &["add-dpu-os", "--password", "example"][..] => false,
            }
        );
    }
}
