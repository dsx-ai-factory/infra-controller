/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::path::PathBuf;

use clap::Parser;
use clap::builder::NonEmptyStringValueParser;

#[derive(Parser, Debug, Clone)]
pub(crate) enum Args {
    #[clap(about = "Set or rotate a firmware artifact access token")]
    Set(SetArgs),
    #[clap(about = "Delete a firmware artifact access token")]
    Delete(DeleteArgs),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a token from a file:
    $ nico-admin-cli credential firmware-access-token set --name repository-a --token-file ./firmware-token.txt

Set a token from standard input:
    $ cat ./firmware-token.txt | nico-admin-cli credential firmware-access-token set --name repository-a --token-file -

")]
pub(crate) struct SetArgs {
    #[arg(
        long,
        help = "Non-secret credential name",
        value_parser = NonEmptyStringValueParser::new()
    )]
    pub(super) name: String,

    #[arg(
        long,
        help = "File containing the token, or '-' to read standard input"
    )]
    pub(super) token_file: PathBuf,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Delete a token:
    $ nico-admin-cli credential firmware-access-token delete --name repository-a

")]
pub(crate) struct DeleteArgs {
    #[arg(
        long,
        help = "Non-secret credential name",
        value_parser = NonEmptyStringValueParser::new()
    )]
    pub(super) name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_name_parser_rejects_empty_values() {
        for (name, valid) in [("repository-a", true), ("", false)] {
            let set = SetArgs::try_parse_from(["set", "--name", name, "--token-file", "token.txt"]);
            let delete = DeleteArgs::try_parse_from(["delete", "--name", name]);

            assert_eq!(set.is_ok(), valid, "set command with name {name:?}");
            assert_eq!(delete.is_ok(), valid, "delete command with name {name:?}");
        }
    }
}
