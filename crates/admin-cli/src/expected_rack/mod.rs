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

mod add;
pub(crate) mod common;
mod delete;
mod erase;
mod replace_all;
mod show;
mod update;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::cli_options::{CliCommand, CliOptions};

    #[test]
    fn mutations_omit_profile() {
        for args in [
            vec!["add", "rack-01"],
            vec!["update", "rack-01", "--meta-name", "rack-01"],
        ] {
            let options = CliOptions::try_parse_from(
                ["nico-admin-cli", "expected-rack"].into_iter().chain(args),
            )
            .unwrap();
            let request: rpc::forge::ExpectedRack = match options.commands.unwrap() {
                CliCommand::ExpectedRack(Cmd::Add(args)) => args.into(),
                CliCommand::ExpectedRack(Cmd::Update(args)) => args.try_into().unwrap(),
                _ => panic!("expected rack mutation"),
            };
            assert_eq!(request.rack_id.unwrap().as_str(), "rack-01");
            assert!(request.rack_profile_id.is_none());
        }
        let imported: common::ExpectedRackJson =
            serde_json::from_str(r#"{"rack_id":"rack-01"}"#).unwrap();
        assert_eq!(imported.rack_id.as_str(), "rack-01");
    }
}

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Show expected rack")]
    Show(show::Args),
    #[clap(about = "Add expected rack")]
    Add(add::Args),
    #[clap(about = "Delete expected rack")]
    Delete(delete::Args),
    #[clap(about = "Update expected rack")]
    Update(update::Args),
    #[clap(about = "Replace all expected racks")]
    ReplaceAll(replace_all::Args),
    #[clap(about = "Erase all expected racks")]
    Erase(erase::Args),
}
