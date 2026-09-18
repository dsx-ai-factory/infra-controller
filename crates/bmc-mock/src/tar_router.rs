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

/// The TAR files used here are a full crawl of a servers' Redfish tree using
/// redfish-mockup-creator.
/// https://github.com/NVIDIA/libredfish/tree/main/tests/mockups
///
/// There is one for each vendor we support in the libredfish repo.
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Buf;
use eyre::Context;
use flate2::read::GzDecoder;
use regex::Regex;

pub(super) type EntryMap = Arc<Mutex<HashMap<String, String>>>;

#[derive(Clone, Default)]
struct TarRouterCache {
    entries: EntryMap,
}

/// Allows callers to specify an in-memory tar (like via include_bytes!()) or a path to one on the
/// filesystem.
pub(super) enum TarGzOption<'a> {
    Disk(&'a PathBuf),
}

impl TarGzOption<'_> {
    fn path(&self) -> Option<&PathBuf> {
        match self {
            TarGzOption::Disk(path) => Some(path),
        }
    }
}

/// Create a mock of
pub(super) fn tar_router(
    targz: TarGzOption,
    existing_tars: Option<&mut HashMap<std::path::PathBuf, EntryMap>>,
) -> eyre::Result<Router> {
    // Check if we've already read this tar.gz
    let maybe_cached_entries = if let Some(existing_tars) = existing_tars.as_ref() {
        targz.path().and_then(|p| existing_tars.get(p).cloned())
    } else {
        None
    };

    let entries = match maybe_cached_entries {
        Some(entries) => entries,
        None => {
            let mut _owned_gz_data = None; // make sure data sent to gz_decoder lives long enough
            let gz_decoder = match targz {
                TarGzOption::Disk(path) => {
                    _owned_gz_data = Some(
                        std::fs::read(path)
                            .wrap_err(format!("cannot read file by path: {path:?}"))?,
                    );
                    GzDecoder::new(_owned_gz_data.as_ref().unwrap().reader())
                }
            };

            let entries = tar::Archive::new(gz_decoder)
                .entries()
                .unwrap()
                .map(Result::unwrap)
                .filter_map(|mut entry| {
                    let name = entry
                        .path()
                        .unwrap()
                        .display()
                        .to_string()
                        .replace("/index.json", "");
                    if name.ends_with('/') {
                        // ignore directories
                        None
                    } else {
                        let mut s = String::with_capacity(entry.size() as usize);
                        let _ = entry.read_to_string(&mut s).unwrap();
                        Some((name, s))
                    }
                })
                .collect::<HashMap<_, _>>();
            let entries = Arc::new(Mutex::new(entries));

            // cache what we just built
            if let (Some(path), Some(existing_tars)) = (targz.path(), existing_tars) {
                existing_tars.insert(path.clone(), entries.clone());
            }

            entries
        }
    };

    let cache = TarRouterCache { entries };

    // Archive mocks answer errors in the same Redfish envelope as generated ones.
    Ok(Router::new()
        .route("/{*path}", get(get_from_tar))
        .fallback(not_found_handler)
        .with_state(cache)
        .layer(axum::middleware::from_fn(super::redfish_error_envelope)))
}

lazy_static::lazy_static! {
    static ref GET_MANAGER_RE: Regex = Regex::new(r#"Managers/[A-Za-z0-9\-_.~]+$"#).unwrap();
    // Match DateTime field in JSON format: "DateTime": "YYYY-MM-DDTHH:MM:SS+/-HH:MM"
    static ref DATETIME_RE: Regex = Regex::new(r#""DateTime":\s*"[^"]+""#).unwrap();
}

/// Read redfish data from the tar
async fn get_from_tar(
    AxumState(cache): AxumState<TarRouterCache>,
    AxumPath(mut path): AxumPath<String>,
) -> Response {
    if path.ends_with('/') {
        path.pop();
    };

    match cache.entries.lock().unwrap().get(&path) {
        None => {
            tracing::trace!(
                path = %path,
                "BMC mock archive path not found",
            );
            (StatusCode::NOT_FOUND, path).into_response()
        }
        Some(s) => (StatusCode::OK, s.clone()).into_response(),
    }
}

// We should never get here, but axum's matchit bug means we sometimes do: https://github.com/tokio-rs/axum/issues/1986
async fn not_found_handler(req: Request<Body>) -> (StatusCode, String) {
    tracing::warn!(
        method = %req.method(),
        uri = %req.uri(),
        "No route for BMC mock request",
    );
    (
        StatusCode::NOT_FOUND,
        format!("No route for {} {}", req.method(), req.uri()),
    )
}
