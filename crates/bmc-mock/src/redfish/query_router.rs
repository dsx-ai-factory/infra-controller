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

//! The collection query parameters of DSP0266 section 7.3 — `$filter`,
//! `$skip`, and `$top` — served on every resource collection by one layer,
//! the way `expander_router` serves `$expand`. Handlers serve collections
//! whole; this layer filters, then pages, and answers unsupported `$`
//! parameters and misuse with the Base registry messages the specification
//! names.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use carbide_axum_utils::router::call_router_with_new_request;
use futures::future::join_all;
use serde_json::{Map, Value};

use crate::http;
use crate::redfish::expander_router::{BufferError, json_bytes};
use crate::redfish::filter::Filter;

/// The `$` parameters some layer of this mock serves. Any other is a 501,
/// as the specification requires; parameters without a `$` are ignored.
const SUPPORTED: [&str; 4] = ["$filter", "$skip", "$top", "$expand"];

/// Entries per page of the collections that page, by `@odata.id`. A
/// collection absent here is served whole unless the client asks for
/// `$top`.
#[derive(Clone, Debug, Default)]
pub(crate) struct PageSizes(Arc<HashMap<String, usize>>);

impl PageSizes {
    pub(crate) fn new(sizes: impl IntoIterator<Item = (String, usize)>) -> Self {
        Self(Arc::new(sizes.into_iter().collect()))
    }

    fn of(&self, odata_id: &str) -> Option<usize> {
        self.0.get(odata_id).copied()
    }
}

pub(crate) fn append(router: Router, page_sizes: PageSizes) -> Router {
    Router::new()
        .route("/{*all}", get(process).fallback(fallback))
        .with_state(Querying {
            inner: router,
            page_sizes,
        })
}

async fn fallback(State(mut state): State<Querying>, request: Request<Body>) -> Response {
    state.call_inner_router(request).await
}

async fn process(State(mut state): State<Querying>, request: Request<Body>) -> Response {
    let params: Vec<(String, String)> = request.uri().query().map_or_else(Vec::new, |query| {
        form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect()
    });
    let query = match Query::parse(&params) {
        Ok(Some(query)) => query,
        // A collection with a page size of its own pages unasked.
        Ok(None) if state.page_sizes.of(request.uri().path()).is_none() => {
            return state.call_inner_router(request).await;
        }
        Ok(None) => Query::default(),
        Err(error) => return error.into_response(),
    };

    let path = request.uri().path().to_owned();
    let response = state.call_inner_router(request).await;
    if !response.status().is_success() {
        return response;
    }
    let (parts, bytes) = match json_bytes(response).await {
        Ok(buffered) => buffered,
        // Collection queries are defined for JSON, never streaming bodies.
        Err(BufferError::NotJson(response)) => return response,
        Err(BufferError::Read(e)) => {
            return http::redfish_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not read the collection: {e}"),
            );
        }
    };
    let Ok(mut document) = serde_json::from_slice::<Map<String, Value>>(&bytes) else {
        return (parts, bytes).into_response();
    };
    let Some(Value::Array(members)) = document.remove("Members") else {
        return QueryError::NotSupportedOnResource.into_response();
    };

    let admitted = match state.admitted(query.filter.as_ref(), members).await {
        Ok(admitted) => admitted,
        Err(message) => return http::redfish_error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    };
    let page_size = document
        .get("@odata.id")
        .and_then(Value::as_str)
        .and_then(|odata_id| state.page_sizes.of(odata_id));
    let page = query.paging.page(admitted, page_size);
    document.insert("Members".to_owned(), Value::Array(page.members));
    document.insert("Members@odata.count".to_owned(), page.total.into());
    if let Some(next_skip) = page.next_skip {
        // The continuation keeps every option but `$skip`, so its pages are
        // pages of the same answer.
        let next_skip = next_skip.to_string();
        let continuation = query_string(
            std::iter::once(&("$skip".to_owned(), next_skip))
                .chain(params.iter().filter(|(key, _)| key != "$skip")),
        );
        document.insert(
            "Members@odata.nextLink".to_owned(),
            Value::String(format!("{path}{continuation}")),
        );
    }

    // Keep the inner response's headers, including its JSON content type.
    let mut parts = parts;
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    (parts, serde_json::to_vec(&document).expect("serde error")).into_response()
}

/// The collection options of one request.
#[derive(Default)]
struct Query {
    filter: Option<Filter>,
    paging: Paging,
}

impl Query {
    /// `None` when the request carries no collection option and can pass
    /// through untouched.
    fn parse(params: &[(String, String)]) -> Result<Option<Self>, QueryError> {
        let mut filter = None;
        let mut paging = Paging::default();
        let mut collection_option = false;
        for (key, value) in params {
            match key.as_str() {
                "$filter" => {
                    filter = Some(Filter::parse(value).map_err(|_| QueryError::ValueFormat {
                        parameter: key.clone(),
                        value: value.clone(),
                    })?);
                }
                "$skip" => paging.skip = Paging::bound(key, value, 0)?,
                "$top" => paging.top = Some(Paging::bound(key, value, 1)?),
                key if key.starts_with('$') && !SUPPORTED.contains(&key) => {
                    return Err(QueryError::Unsupported {
                        parameter: key.to_owned(),
                    });
                }
                _ => continue,
            }
            collection_option = true;
        }
        Ok(collection_option.then_some(Self { filter, paging }))
    }
}

/// `$skip` and `$top`, applied after `$filter`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Paging {
    skip: usize,
    top: Option<usize>,
}

struct Page<T> {
    members: Vec<T>,
    /// Members after `$filter`, as `Members@odata.count`.
    total: usize,
    /// `$skip` of the next page, when this one did not reach the end.
    next_skip: Option<usize>,
}

impl Paging {
    /// An integer option no smaller than `least`. `$top=0` is out of range
    /// because a zero-member page makes no progress: its continuation would
    /// point at itself.
    fn bound(parameter: &str, value: &str, least: i64) -> Result<usize, QueryError> {
        let number: i64 = value.parse().map_err(|_| QueryError::ValueFormat {
            parameter: parameter.to_owned(),
            value: value.to_owned(),
        })?;
        usize::try_from(number)
            .ok()
            .filter(|_| number >= least)
            .ok_or_else(|| QueryError::OutOfRange {
                parameter: parameter.to_owned(),
                value: value.to_owned(),
                range: format!("{least} or more"),
            })
    }

    /// This page of `members`. `$top` is capped at `page_size`, which also
    /// pages when `$top` is absent; without one, everything from `$skip` is
    /// served at once.
    fn page<T>(&self, members: Vec<T>, page_size: Option<usize>) -> Page<T> {
        let total = members.len();
        let limit = match (self.top, page_size) {
            (Some(top), Some(page)) => Some(top.min(page)),
            (Some(top), None) => Some(top),
            (None, page) => page,
        };
        let members: Vec<T> = members
            .into_iter()
            .skip(self.skip)
            .take(limit.unwrap_or(usize::MAX))
            .collect();
        let served = self.skip.saturating_add(members.len());
        let next_skip = (limit.is_some() && served < total).then_some(served);
        Page {
            members,
            total,
            next_skip,
        }
    }
}

/// The Base message registry answers DSP0266 section 7.3 prescribes.
#[derive(Debug, PartialEq, Eq)]
enum QueryError {
    /// A `$` parameter nothing here serves: 501.
    Unsupported {
        parameter: String,
    },
    /// A collection option on a resource that is not a collection.
    NotSupportedOnResource,
    ValueFormat {
        parameter: String,
        value: String,
    },
    OutOfRange {
        parameter: String,
        value: String,
        range: String,
    },
}

impl IntoResponse for QueryError {
    fn into_response(self) -> Response {
        const BASE: &str = "Base.1.19";
        match self {
            Self::Unsupported { parameter } => http::registry_error(
                StatusCode::NOT_IMPLEMENTED,
                &format!("{BASE}.QueryParameterUnsupported"),
                &format!("Query parameter '{parameter}' is not supported."),
                &[&parameter],
                "Remove the query parameter and resubmit the request if the operation failed.",
            ),
            Self::NotSupportedOnResource => http::registry_error(
                StatusCode::BAD_REQUEST,
                &format!("{BASE}.QueryNotSupportedOnResource"),
                "Querying is not supported on the requested resource.",
                &[],
                "Remove the query parameters and resubmit the request if the operation failed.",
            ),
            Self::ValueFormat { parameter, value } => http::registry_error(
                StatusCode::BAD_REQUEST,
                &format!("{BASE}.QueryParameterValueFormatError"),
                &format!(
                    "The value '{value}' for the parameter '{parameter}' is not in the correct format."
                ),
                &[&value, &parameter],
                "Correct the value for the query parameter in the request and resubmit the request if the operation failed.",
            ),
            Self::OutOfRange {
                parameter,
                value,
                range,
            } => http::registry_error(
                StatusCode::BAD_REQUEST,
                &format!("{BASE}.QueryParameterOutOfRange"),
                &format!(
                    "The value '{value}' for the query parameter '{parameter}' is out of range {range}."
                ),
                &[&value, &parameter, &range],
                "Reduce the value for the query parameter to a value that is within range, such as a start or count value that is within bounds of the number of resources in a collection or a page that is within the range of valid pages.",
            ),
        }
    }
}

/// `?key=value&...` with values encoded and Redfish's `$` keys left legible,
/// or nothing when there are no options.
fn query_string<'a>(params: impl Iterator<Item = &'a (String, String)>) -> String {
    let mut query = String::new();
    for (key, value) in params {
        query.push(if query.is_empty() { '?' } else { '&' });
        query.push_str(key);
        query.push('=');
        query.extend(form_urlencoded::byte_serialize(value.as_bytes()));
    }
    query
}

/// A `{ "@odata.id": ... }` reference, as opposed to a member served inline.
fn reference(member: &Value) -> Option<&str> {
    let object = member.as_object()?;
    (object.len() == 1)
        .then(|| object.get("@odata.id"))
        .flatten()?
        .as_str()
}

#[derive(Debug, Clone)]
struct Querying {
    inner: Router,
    page_sizes: PageSizes,
}

impl Querying {
    /// See docs in `call_router_with_new_request`
    async fn call_inner_router(&mut self, request: Request<Body>) -> Response {
        call_router_with_new_request(&mut self.inner, request).await
    }

    async fn get_json(&mut self, uri: &str) -> Result<Map<String, Value>, String> {
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .map_err(|e| format!("`{uri}` is not a request URI: {e}"))?;
        let response = self.call_inner_router(request).await;
        let (parts, bytes) = match json_bytes(response).await {
            Ok(buffered) => buffered,
            Err(BufferError::NotJson(response)) => {
                return Err(format!("`{uri}` answered a non-JSON {}", response.status()));
            }
            Err(BufferError::Read(e)) => return Err(format!("could not read `{uri}`: {e}")),
        };
        if !parts.status.is_success() {
            return Err(format!(
                "`{uri}` answered {}: {}",
                parts.status,
                String::from_utf8_lossy(&bytes)
            ));
        }
        serde_json::from_slice(&bytes).map_err(|e| format!("`{uri}` is not a JSON object: {e}"))
    }

    /// The members `filter` admits, in order. A member served as a reference
    /// is read from the inner router to be judged, and stays a reference.
    async fn admitted(
        &self,
        filter: Option<&Filter>,
        members: Vec<Value>,
    ) -> Result<Vec<Value>, String> {
        let Some(filter) = filter else {
            return Ok(members);
        };
        let judged = join_all(members.into_iter().map(|member| {
            let mut state = self.clone();
            async move {
                let admitted = match reference(&member) {
                    Some(uri) => filter.admits(&Value::Object(state.get_json(uri).await?)),
                    None => filter.admits(&member),
                };
                Ok::<_, String>(admitted.then_some(member))
            }
        }))
        .await;
        judged
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map(|members| members.into_iter().flatten().collect())
            .map_err(|message| format!("could not read a collection member: {message}"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use carbide_test_support::{Check, check_values};
    use tower::ServiceExt;

    use super::*;
    use crate::bmc_state::BmcState;
    use crate::redfish::log_service::LogEntryDraft;
    use crate::test_support::{NoopCallbacks, host_info};
    use crate::{HardwareType, MachineRouterOptions, machine_router};

    const SYSTEM: &str = "/redfish/v1/Systems/System.Embedded.1";
    const ENTRIES: &str = "/redfish/v1/Systems/System.Embedded.1/LogServices/EventLog/Entries";
    const ADAPTERS: &str = "/redfish/v1/Chassis/System.Embedded.1/NetworkAdapters";
    /// `Created` of the Dell profile's seeded entry.
    const SEED: &str = "2026-02-12T02:06:58Z";

    fn dell_router() -> (Router, BmcState) {
        machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            String::new(),
            false,
            MachineRouterOptions::default(),
        )
    }

    async fn get(router: &Router, path: &str) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn get_ok(router: &Router, path: &str) -> serde_json::Value {
        let (status, body) = get(router, path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        body
    }

    #[test]
    fn paging_arithmetic() {
        struct Case {
            scenario: &'static str,
            paging: Paging,
            page_size: Option<usize>,
            expect: (Vec<u8>, usize, Option<usize>),
        }
        for case in [
            Case {
                scenario: "the collection's page size pages an unqualified request",
                paging: Paging::default(),
                page_size: Some(2),
                expect: (vec![0, 1], 5, Some(2)),
            },
            Case {
                scenario: "$top cannot exceed the page size",
                paging: Paging {
                    skip: 0,
                    top: Some(4),
                },
                page_size: Some(2),
                expect: (vec![0, 1], 5, Some(2)),
            },
            Case {
                scenario: "the last page has no continuation",
                paging: Paging { skip: 4, top: None },
                page_size: Some(2),
                expect: (vec![4], 5, None),
            },
            Case {
                scenario: "$skip past the end is empty",
                paging: Paging { skip: 9, top: None },
                page_size: Some(2),
                expect: (vec![], 5, None),
            },
            Case {
                scenario: "an unpaged collection is served whole",
                paging: Paging::default(),
                page_size: None,
                expect: (vec![0, 1, 2, 3, 4], 5, None),
            },
            Case {
                scenario: "a client-supplied $top still pages an unpaged collection",
                paging: Paging {
                    skip: 0,
                    top: Some(2),
                },
                page_size: None,
                expect: (vec![0, 1], 5, Some(2)),
            },
        ] {
            let page = case.paging.page((0..5).collect(), case.page_size);
            assert_eq!(
                (page.members, page.total, page.next_skip),
                case.expect,
                "{}",
                case.scenario
            );
        }
    }

    #[test]
    fn query_parameters_are_validated_as_the_specification_asks() {
        let parse = |query: &str| {
            let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
            Query::parse(&params).map(|query| query.map(|query| query.paging))
        };
        let bad = |parameter: &str, value: &str| QueryError::ValueFormat {
            parameter: parameter.to_owned(),
            value: value.to_owned(),
        };
        check_values(
            [
                Check {
                    scenario: "no collection option passes through",
                    input: "only&excerpt&vendor=1",
                    expect: Ok(None),
                },
                Check {
                    scenario: "$expand alone is the expander's",
                    input: "$expand=*",
                    expect: Ok(None),
                },
                Check {
                    scenario: "paging options",
                    input: "$skip=2&$top=5",
                    expect: Ok(Some(Paging {
                        skip: 2,
                        top: Some(5),
                    })),
                },
                Check {
                    scenario: "an unsupported $ parameter is refused",
                    input: "$select=Id",
                    expect: Err(QueryError::Unsupported {
                        parameter: "$select".to_owned(),
                    }),
                },
                Check {
                    scenario: "a non-integer $skip",
                    input: "$skip=many",
                    expect: Err(bad("$skip", "many")),
                },
                Check {
                    scenario: "a negative $skip",
                    input: "$skip=-1",
                    expect: Err(QueryError::OutOfRange {
                        parameter: "$skip".to_owned(),
                        value: "-1".to_owned(),
                        range: "0 or more".to_owned(),
                    }),
                },
                Check {
                    scenario: "$top=0 would never make progress",
                    input: "$top=0",
                    expect: Err(QueryError::OutOfRange {
                        parameter: "$top".to_owned(),
                        value: "0".to_owned(),
                        range: "1 or more".to_owned(),
                    }),
                },
                Check {
                    scenario: "a $filter the grammar does not cover",
                    input: "$filter=Created%20ge%20yesterday",
                    expect: Err(bad("$filter", "Created ge yesterday")),
                },
            ],
            parse,
        );
    }

    #[tokio::test]
    async fn inline_members_are_filtered_and_paged_as_one_answer() {
        let (router, state) = dell_router();
        let root = get_ok(&router, "/redfish/v1").await;
        assert_eq!(root["ProtocolFeaturesSupported"]["FilterQuery"], true);

        // The seed is stamped 2026-02-12; the three lifecycle entries now.
        for _ in 0..3 {
            state.record_log(LogEntryDraft::powered_on(SYSTEM));
        }
        let resumed = get_ok(
            &router,
            &format!("{ENTRIES}?$filter=Created%20gt%20'{SEED}'%20and%20Severity%20eq%20'OK'"),
        )
        .await;
        assert_eq!(
            resumed["Members@odata.count"], 3,
            "the count is of what matched"
        );
        assert_eq!(resumed["Members"][0]["Id"], "1");
        // A `+` a client left unencoded decodes as a space and is still an offset.
        let from_seed = get_ok(
            &router,
            &format!("{ENTRIES}?$filter=Created%20ge%202026-02-12T02:06:58+00:00"),
        )
        .await;
        assert_eq!(from_seed["Members@odata.count"], 4);

        // A filtered page continues under the same filter.
        let paged = get_ok(
            &router,
            &format!("{ENTRIES}?$top=2&$filter=Created%20gt%20'{SEED}'"),
        )
        .await;
        assert_eq!(paged["Members"].as_array().unwrap().len(), 2);
        let next = paged["Members@odata.nextLink"].as_str().unwrap();
        assert_eq!(
            next,
            format!("{ENTRIES}?$skip=2&$top=2&$filter=Created+gt+%272026-02-12T02%3A06%3A58Z%27")
        );
        let rest = get_ok(&router, next).await;
        assert_eq!(rest["Members"].as_array().unwrap().len(), 1);
        assert!(rest.get("Members@odata.nextLink").is_none());

        // The Dell profile pages fifty at a time, filtered or not.
        for _ in 0..60 {
            state.record_log(LogEntryDraft::powered_on(SYSTEM));
        }
        let capped = get_ok(
            &router,
            &format!("{ENTRIES}?$top=100&$filter=Created%20gt%20'{SEED}'"),
        )
        .await;
        assert_eq!(
            (
                capped["Members"].as_array().unwrap().len(),
                &capped["Members@odata.count"]
            ),
            (50, &63.into())
        );
    }

    #[tokio::test]
    async fn referenced_members_are_judged_by_their_resource() {
        let (router, _) = dell_router();
        let all = get_ok(&router, ADAPTERS).await["Members@odata.count"]
            .as_u64()
            .unwrap();
        assert!(all > 1);
        let embedded = "$filter=Manufacturer%20eq%20'Broadcom%20Inc.%20and%20subsidiaries'";
        let broadcom = get_ok(&router, &format!("{ADAPTERS}?{embedded}")).await;
        assert_eq!(
            broadcom["Members"],
            serde_json::json!([{"@odata.id": format!("{ADAPTERS}/NIC.Embedded.1")}]),
            "members stay references"
        );
        assert_eq!(broadcom["Members@odata.count"], 1);
        let others = get_ok(
            &router,
            &format!("{ADAPTERS}?$filter=not%20Manufacturer%20eq%20'Broadcom%20Inc.%20and%20subsidiaries'"),
        )
        .await;
        assert_eq!(others["Members@odata.count"], all - 1);

        // Paging an unpaged collection, and `$expand` of the filtered result.
        let one = get_ok(&router, &format!("{ADAPTERS}?$top=1&$skip=1")).await;
        assert_eq!(one["Members"].as_array().unwrap().len(), 1);
        assert_eq!(one["Members@odata.count"], all);
        let expanded = get_ok(
            &router,
            &format!("{ADAPTERS}?{embedded}&$expand=.($levels=1)"),
        )
        .await;
        assert_eq!(expanded["Members"].as_array().unwrap().len(), 1);
        assert_eq!(
            expanded["Members"][0]["Manufacturer"],
            "Broadcom Inc. and subsidiaries"
        );
    }

    #[tokio::test]
    async fn misuse_is_answered_with_registry_messages() {
        let (router, _) = dell_router();
        let singleton = format!("{ADAPTERS}/NIC.Embedded.1");
        for (path, status, message_id) in [
            (
                format!("{singleton}?$skip=1"),
                StatusCode::BAD_REQUEST,
                "Base.1.19.QueryNotSupportedOnResource",
            ),
            (
                format!("{ADAPTERS}?$select=Id"),
                StatusCode::NOT_IMPLEMENTED,
                "Base.1.19.QueryParameterUnsupported",
            ),
            (
                format!("{ADAPTERS}?$top=0"),
                StatusCode::BAD_REQUEST,
                "Base.1.19.QueryParameterOutOfRange",
            ),
            (
                format!("{ENTRIES}?$filter=Created%20ge%20yesterday"),
                StatusCode::BAD_REQUEST,
                "Base.1.19.QueryParameterValueFormatError",
            ),
        ] {
            let (got, body) = get(&router, &path).await;
            assert_eq!(got, status, "{path}: {body}");
            assert_eq!(body["error"]["code"], message_id, "{path}");
            assert_eq!(
                body["error"]["@Message.ExtendedInfo"][0]["MessageId"],
                message_id
            );
        }
        // The inner answer stands when the resource is missing.
        let (status, _) = get(&router, "/redfish/v1/Chassis/Nope?$skip=1").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
