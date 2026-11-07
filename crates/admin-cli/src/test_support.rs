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

//! Test-only helpers for command-line parsing and mock gRPC responses.
//! Parsing helpers avoid exposing implementation fields outside their owning modules.
//!
//! Prefer typed [`ArgMatches`] accessors when the parsed value's type is part
//! of the assertion. The raw-value helpers are intended for fields that are
//! genuinely strings.

use std::convert::Infallible;

use clap::error::ErrorKind;
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use futures::stream;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::{Response, header};
use prost::Message;

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

/// Encodes a successful unary gRPC response for a mock server.
pub(crate) fn grpc_response(message: impl Message) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
    let mut data = vec![0];
    data.extend_from_slice(&u32::try_from(message.encoded_len()).unwrap().to_be_bytes());
    message.encode(&mut data).unwrap();
    let mut trailers = hyper::HeaderMap::new();
    trailers.insert("grpc-status", header::HeaderValue::from_static("0"));
    let body = StreamBody::new(stream::iter([
        Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
        Ok(Frame::trailers(trailers)),
    ]))
    .boxed_unsync();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(body)
        .unwrap()
}
