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

#[derive(Parser, Debug, Clone)]
pub(crate) enum Args {
    #[clap(about = "Set credentials for a container registry")]
    Set(SetArgs),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set credentials for the NGC staging registry:
    $ read -r -s -p 'Registry token: ' registry_token; printf '\\n'
    $ printf '%s' \"$registry_token\" | nico-admin-cli credential registry set \
    --registry nvcr.io --username '$oauthtoken' --password-stdin
    $ unset registry_token

")]
pub(crate) struct SetArgs {
    #[clap(long, help = "Registry hostname (e.g. nvcr.io)")]
    pub(super) registry: String,
    #[clap(long, help = "Registry username")]
    pub(super) username: String,
    #[clap(
        long,
        required = true,
        help = "Read the registry password or API key from standard input; it is never accepted in command arguments"
    )]
    pub(super) password_stdin: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::SetArgs;

    #[test]
    fn requires_password_from_standard_input() {
        assert!(
            SetArgs::try_parse_from([
                "registry-set",
                "--registry",
                "registry.example.com",
                "--username",
                "registry-user",
            ])
            .is_err()
        );
    }

    #[test]
    fn accepts_password_from_standard_input() {
        let args = SetArgs::try_parse_from([
            "registry-set",
            "--registry",
            "registry.example.com",
            "--username",
            "registry-user",
            "--password-stdin",
        ])
        .expect("--password-stdin should satisfy the password requirement");
        assert!(args.password_stdin);
    }
}
