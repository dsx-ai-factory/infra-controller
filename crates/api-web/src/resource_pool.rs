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

use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::State as AxumState;
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::Base;

#[derive(Template)]
#[template(path = "resource_pool_show.html")]
struct ResourcePoolShow {
    pools: Vec<ResourcePoolRow>,
}

/// `ResourcePoolRow` keeps absent counts visible in the table instead of
/// turning them into zeroes or percentages of an unknown total.
struct ResourcePoolRow {
    name: String,
    backend: String,
    remote_pool: String,
    min: String,
    max: String,
    total: String,
    used: String,
    status: String,
}

impl From<forgerpc::ResourcePoolInspection> for ResourcePoolRow {
    fn from(pool: forgerpc::ResourcePoolInspection) -> Self {
        let backend = match pool.backend_kind() {
            forgerpc::ResourcePoolBackendKind::Integrated => "Integrated",
            forgerpc::ResourcePoolBackendKind::Grpc => "gRPC",
            forgerpc::ResourcePoolBackendKind::Unspecified => "Unknown",
        };
        let backend = match &pool.backend_name {
            Some(name) => format!("{backend} ({name})"),
            None => backend.to_string(),
        };

        let (total, used, status) = match (pool.stats_status(), &pool.stats) {
            (forgerpc::ResourcePoolStatsStatus::Known, Some(stats)) => {
                match stats.used.checked_add(stats.free) {
                    Some(total) => {
                        let used = if total == 0 {
                            stats.used.to_string()
                        } else {
                            format!(
                                "{} ({:.0}%)",
                                stats.used,
                                stats.used as f64 / total as f64 * 100.0
                            )
                        };
                        (total.to_string(), used, "Known")
                    }
                    None => ("Unavailable".into(), "Unavailable".into(), "Unavailable"),
                }
            }
            (forgerpc::ResourcePoolStatsStatus::Unknown, _) => {
                ("Unknown".into(), "Unknown".into(), "Unknown")
            }
            _ => ("Unavailable".into(), "Unavailable".into(), "Unavailable"),
        };
        let status = match pool.stats_message {
            Some(message) => format!("{status}: {message}"),
            None => status.to_string(),
        };

        Self {
            name: pool.name,
            backend,
            remote_pool: pool.remote_pool,
            min: pool.min.unwrap_or_else(|| "—".into()),
            max: pool.max.unwrap_or_else(|| "—".into()),
            total,
            used,
            status,
        }
    }
}

/// `show_html` lists pool identities and the counts available from each backend.
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let pools = match fetch_resource_pools(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "admin_inspect_resource_pools");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed loading resource pools",
            )
                .into_response();
        }
    };
    let tmpl = ResourcePoolShow {
        pools: pools.into_iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

/// `show_all_json` preserves the existing numeric response for Integrated pools.
/// A saved remote binding returns HTTP 412 because this schema cannot express
/// unknown counts. Backend status is available through the gRPC
/// `AdminInspectResourcePools` method.
pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let request = tonic::Request::new(forgerpc::ListResourcePoolsRequest {
        auto_assignable: None,
    });
    let pools = match state.admin_list_resource_pools(request).await {
        Ok(response) => response.into_inner().pools,
        Err(err) if err.code() == tonic::Code::FailedPrecondition => {
            return (StatusCode::PRECONDITION_FAILED, err.message().to_string()).into_response();
        }
        Err(err) => {
            tracing::error!(error = %err, "admin_list_resource_pools");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed loading resource pools",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(pools)).into_response()
}

/// `fetch_resource_pools` uses the inspection API so a remote pool remains
/// visible when it has no local inventory or cannot report its capacity.
async fn fetch_resource_pools(
    api: Arc<Api>,
) -> Result<Vec<forgerpc::ResourcePoolInspection>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::InspectResourcePoolsRequest {});
    let out = api
        .admin_inspect_resource_pools(request)
        .await
        .map(|response| response.into_inner())?;
    Ok(out.pools)
}

impl super::Base for ResourcePoolShow {}

#[cfg(test)]
mod tests {
    use askama::Template;
    use carbide_test_support::{Check, check_values};
    use rpc::forge;

    use super::ResourcePoolShow;

    /// Unsupported statistics and overflowing totals must remain visible as
    /// missing counts, without zeroes or usage percentages.
    #[test]
    fn inspection_template_preserves_missing_capacity() {
        check_values(
            [
                Check {
                    scenario: "provider does not support statistics",
                    input: (forge::ResourcePoolStatsStatus::Unknown, None),
                    expect: (vec!["Unknown".to_string(); 3], 2, false, false),
                },
                Check {
                    scenario: "known counts overflow their displayed total",
                    input: (
                        forge::ResourcePoolStatsStatus::Known,
                        Some(forge::ResourcePoolStats {
                            used: u64::MAX,
                            free: 1,
                            ..Default::default()
                        }),
                    ),
                    expect: (vec!["Unavailable".to_string(); 3], 2, false, false),
                },
            ],
            |(status, stats)| {
                let page = ResourcePoolShow {
                    pools: vec![
                        forge::ResourcePoolInspection {
                            name: "remote-vnis".to_string(),
                            backend_kind: forge::ResourcePoolBackendKind::Grpc as i32,
                            remote_pool: "tenant-vnis".to_string(),
                            stats_status: status as i32,
                            stats,
                            ..Default::default()
                        }
                        .into(),
                    ],
                }
                .render()
                .unwrap();
                // The last three cells are Size, Used, and Status.
                let counts: Vec<_> = page
                    .split("<td>")
                    .skip(6)
                    .map(|cell| cell.split("</td>").next().unwrap().to_string())
                    .collect();
                (
                    counts,
                    page.matches("<td>—</td>").count(),
                    page.contains("(0%)"),
                    page.contains("NaN"),
                )
            },
        );
    }
}
