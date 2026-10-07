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

//! Test-only helpers for exercising command-line parsing without exposing
//! implementation fields outside the modules that own them.
//!
//! Prefer typed [`ArgMatches`] accessors when the parsed value's type is part
//! of the assertion. The raw-value helpers are intended for fields that are
//! genuinely strings.

use clap::error::ErrorKind;
use clap::{ArgMatches, CommandFactory, FromArgMatches};

/// Parses `argv`, validates that it constructs `T`, and returns the matches at
/// the requested nested subcommand path.
pub(crate) fn parse_leaf<T>(argv: &[&str], path: &[&str]) -> Result<ArgMatches, clap::Error>
where
    T: CommandFactory + FromArgMatches,
{
    parse_with_leaf_matches::<T>(argv, path).map(|(_, matches)| matches)
}

/// Parses `argv` and returns both the typed command and the matches at the
/// requested nested subcommand path.
///
/// The typed value lets a test verify variant routing independently of the
/// input strings, while the leaf matches let it inspect fields whose owning
/// modules intentionally keep them private.
pub(crate) fn parse_with_leaf_matches<T>(
    argv: &[&str],
    path: &[&str],
) -> Result<(T, ArgMatches), clap::Error>
where
    T: CommandFactory + FromArgMatches,
{
    let matches = T::command().try_get_matches_from(argv.iter().copied())?;
    let command = T::from_arg_matches(&matches)?;
    let matches = leaf_matches(matches, path)?;
    Ok((command, matches))
}

fn leaf_matches(mut matches: ArgMatches, path: &[&str]) -> Result<ArgMatches, clap::Error> {
    for expected in path {
        let Some((actual, child)) = matches.remove_subcommand() else {
            return Err(clap::Error::raw(
                ErrorKind::MissingSubcommand,
                format!("expected `{expected}` subcommand"),
            ));
        };
        if actual != *expected {
            return Err(clap::Error::raw(
                ErrorKind::InvalidSubcommand,
                format!("expected `{expected}` subcommand, found `{actual}`"),
            ));
        }
        matches = child;
    }
    Ok(matches)
}

/// Returns the first raw value for a string-valued argument.
pub(crate) fn raw_value(matches: &ArgMatches, id: &str) -> Option<String> {
    matches
        .get_raw(id)
        .and_then(|mut values| values.next())
        .map(|value| value.to_string_lossy().into_owned())
}

/// Returns all raw values for a string-valued argument.
pub(crate) fn raw_values(matches: &ArgMatches, id: &str) -> Vec<String> {
    matches
        .get_raw(id)
        .into_iter()
        .flatten()
        .map(|value| value.to_string_lossy().into_owned())
        .collect()
}
