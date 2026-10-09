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

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

use http::{Method, StatusCode};
use serde_json::{Map, Value as JsonValue, json};

use super::super::{MockRequest, MockResponse};
use super::NvueMockHandler;

/// A mock handler for configuration revision storage.
///
/// This doesn't try to behave exactly like NVUE. We don't validate configs,
/// and we don't try to model the "pending" config (this can be customized in an
/// override for a test that needs it).
#[derive(Debug)]
pub(crate) struct ConfigRevisionHandler {
    state: Mutex<RevisionState>,
}

impl ConfigRevisionHandler {
    const CONFIG_PATH: &str = "/nvue_v1/";
    const REVISION_COLLECTION_PATH: &str = "/nvue_v1/revision";
    const REVISION_ITEM_ROUTE: &str = "/nvue_v1/revision/{revision_id}";
    const APPLIED_REVISION: &str = "applied";

    /// Create a handler with `applied_config` as the current configuration.
    pub(crate) fn new(applied_config: JsonValue) -> Self {
        const INITIAL_REVISION_ID: &str = "0";

        let revisions = BTreeMap::from([(
            INITIAL_REVISION_ID.to_string(),
            Revision {
                config: applied_config,
            },
        )]);
        Self {
            state: Mutex::new(RevisionState {
                next_revision: 1,
                applied_revision: INITIAL_REVISION_ID.to_string(),
                revisions,
            }),
        }
    }

    /// Return a snapshot of the currently applied configuration.
    pub(crate) fn get_applied_config(&self) -> JsonValue {
        self.get_revision_config(Self::APPLIED_REVISION)
            .expect("applied revision should exist")
    }

    /// Return a snapshot of the configuration for `revision_id`.
    ///
    /// The symbolic `"applied"` revision resolves to the currently applied revision.
    pub(crate) fn get_revision_config(&self, revision_id: &str) -> Option<JsonValue> {
        let state = self.lock_state();
        config_for_revision(&state, revision_id).cloned()
    }

    fn lock_state(&self) -> MutexGuard<'_, RevisionState> {
        self.state.lock().expect("revision state lock should work")
    }

    fn handle_create_revision(&self, request: &MockRequest) -> Option<MockResponse> {
        if request.method != Method::POST
            || !request.query_pairs().is_empty()
            || !request.body.is_empty()
        {
            return bad_request("revision creation requires POST with no query or body");
        }

        let mut state = self.lock_state();
        let revision_id = state.next_revision.to_string();
        state.next_revision += 1;
        let config = config_for_revision(&state, Self::APPLIED_REVISION)
            .expect("applied revision should exist")
            .clone();
        state
            .revisions
            .insert(revision_id.clone(), Revision { config });
        let body = Map::from_iter([(revision_id, json!({}))]);
        Some(MockResponse::json(StatusCode::CREATED, &body))
    }

    fn handle_config(&self, request: &MockRequest) -> Option<MockResponse> {
        let Some(query) = unique_query(request) else {
            return bad_request("configuration request contains duplicate query parameters");
        };

        match request.method {
            Method::DELETE | Method::PATCH => self.handle_config_update(request, &query),
            Method::GET => self.handle_diff(request, &query),
            _ => bad_request("unsupported method for configuration request"),
        }
    }

    fn handle_config_update(
        &self,
        request: &MockRequest,
        query: &HashMap<String, String>,
    ) -> Option<MockResponse> {
        let Some(revision_id) = single_query_value(query, "rev") else {
            return bad_request("staged configuration request requires only a rev parameter");
        };

        let config = match request.method {
            Method::DELETE => match request.json::<JsonValue>() {
                Ok(JsonValue::Object(body)) if body.is_empty() => json!({}),
                _ => return bad_request("revision delete body must be an empty JSON object"),
            },
            Method::PATCH => match request.json::<JsonValue>() {
                Ok(config @ JsonValue::Object(_)) => config,
                _ => return bad_request("revision patch body must be a JSON object"),
            },
            _ => unreachable!("caller restricts staged configuration methods"),
        };

        let mut state = self.lock_state();
        let Some(revision) = state.revisions.get_mut(revision_id) else {
            return not_found(revision_id);
        };
        revision.config = config;
        Some(MockResponse::empty(StatusCode::OK))
    }

    fn handle_diff(
        &self,
        request: &MockRequest,
        query: &HashMap<String, String>,
    ) -> Option<MockResponse> {
        if !request.body.is_empty()
            || query.len() != 3
            || query.get("filled").map(String::as_str) != Some("false")
        {
            return bad_request("diff request requires diff, rev, and filled=false");
        }
        let (Some(base_revision), Some(target_revision)) = (query.get("diff"), query.get("rev"))
        else {
            return bad_request("diff request requires diff and rev parameters");
        };

        let state = self.lock_state();
        let Some(base_config) = config_for_revision(&state, base_revision) else {
            return not_found(base_revision);
        };
        let Some(target_config) = config_for_revision(&state, target_revision) else {
            return not_found(target_revision);
        };
        let body = if base_config == target_config {
            json!({})
        } else {
            // Note that this doesn't match what NVUE does, but it's exactly
            // sufficient for a caller that's only checking for an empty response.
            // If we ever have diff users that read the response, we'll need to mock
            // this more carefully.
            json!({"changed": true})
        };
        Some(MockResponse::json(StatusCode::OK, &body))
    }

    fn handle_revision(&self, request: &MockRequest, revision_id: &str) -> Option<MockResponse> {
        if !request.query_pairs().is_empty() {
            return bad_request("revision item requests do not accept query parameters");
        }

        match request.method {
            Method::GET => {
                if !request.body.is_empty() {
                    return bad_request("revision status requests do not accept a body");
                }
                let state = self.lock_state();
                let resolved_revision_id = resolve_revision_id(&state, revision_id);
                if !state.revisions.contains_key(resolved_revision_id) {
                    return not_found(revision_id);
                }
                Some(revision_response(
                    resolved_revision_id,
                    &state.applied_revision,
                ))
            }
            Method::PATCH => self.handle_apply(request, revision_id),
            _ => bad_request("unsupported method for revision item request"),
        }
    }

    fn handle_apply(&self, request: &MockRequest, revision_id: &str) -> Option<MockResponse> {
        if !valid_apply_body(request) {
            return bad_request("revision apply body is malformed");
        }

        let mut state = self.lock_state();
        if !state.revisions.contains_key(revision_id) {
            return not_found(revision_id);
        }
        state.applied_revision = revision_id.to_string();
        Some(revision_response(revision_id, &state.applied_revision))
    }
}

impl Default for ConfigRevisionHandler {
    fn default() -> Self {
        Self::new(json!({}))
    }
}

impl NvueMockHandler for ConfigRevisionHandler {
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        if request.uri.path() == Self::REVISION_COLLECTION_PATH {
            return self.handle_create_revision(request);
        }
        if request.uri.path() == Self::CONFIG_PATH {
            return self.handle_config(request);
        }
        if let Some(captures) =
            request.route_captures(request.method.clone(), Self::REVISION_ITEM_ROUTE)
        {
            return self.handle_revision(request, &captures["revision_id"]);
        }
        None
    }
}

/// Models an NVUE server that models successfully creating revision but then
/// failing to apply it, including the rollback logic.
pub(crate) struct ConfigRevisionRollbackHandler {
    storage: ConfigRevisionHandler,
    state: Mutex<Option<RollbackAttempt>>,
    apply_checking_polls: usize,
    rollback_checking_polls: usize,
}

struct RollbackAttempt {
    revision_id: String,
    apply_checking_remaining: usize,
    rollback_checking_remaining: usize,
    rollback_advertised: bool,
    rollback_completed: bool,
}

impl RollbackAttempt {
    fn rollback_id(&self) -> String {
        format!("rev_{}_apply_1/start", self.revision_id)
    }
}

impl ConfigRevisionRollbackHandler {
    /// Start with `applied_config`, returning `checking` for the specified polls
    /// before the apply fails and its rollback succeeds, respectively.
    pub(crate) fn new(
        applied_config: JsonValue,
        apply_checking_polls: usize,
        rollback_checking_polls: usize,
    ) -> Self {
        Self {
            storage: ConfigRevisionHandler::new(applied_config),
            state: Mutex::new(None),
            apply_checking_polls,
            rollback_checking_polls,
        }
    }

    /// Return the configuration applied before any failed attempt.
    pub(crate) fn get_applied_config(&self) -> JsonValue {
        self.storage.get_applied_config()
    }

    fn handle_apply(&self, request: &MockRequest, revision_id: &str) -> Option<MockResponse> {
        if request.uri.query().is_some() {
            return self.storage.handle(request);
        }
        if !valid_apply_body(request) {
            return bad_request("revision apply body is malformed");
        }
        let mut state = self.state.lock().expect("rollback state lock should work");
        if state.is_some() {
            return bad_request("cannot repeat or overlap failed applies");
        }
        let storage = self.storage.lock_state();
        if !storage.revisions.contains_key(revision_id) {
            return not_found(revision_id);
        }
        if storage.applied_revision == revision_id {
            return bad_request("cannot apply the initial revision");
        }
        *state = Some(RollbackAttempt {
            revision_id: revision_id.to_string(),
            apply_checking_remaining: self.apply_checking_polls,
            rollback_checking_remaining: self.rollback_checking_polls,
            rollback_advertised: false,
            rollback_completed: false,
        });
        Some(MockResponse::json(
            StatusCode::OK,
            &json!({"state": "apply"}),
        ))
    }

    fn handle_revision_read(&self, revision_id: &str) -> Option<MockResponse> {
        let mut state = self.state.lock().expect("rollback state lock should work");
        let attempt = state.as_mut()?;
        let revision = if revision_id == attempt.revision_id {
            if attempt.apply_checking_remaining > 0 {
                attempt.apply_checking_remaining -= 1;
                json!({"state": "checking", "transition": {"progress": "applying"}})
            } else {
                attempt.rollback_advertised = true;
                json!({
                    "state": "apply_fail",
                    "transition": {
                        "progress": "apply failed",
                        "issue": {
                            "1": {"severity": "error", "code": "apply_failed", "message": "apply failed", "data": {"path": "/system"}},
                            "2": {"severity": "warning", "code": "rollback", "data": {"rollback_target": attempt.rollback_id()}},
                        },
                    },
                })
            }
        } else if revision_id == attempt.rollback_id() && attempt.rollback_advertised {
            if attempt.rollback_checking_remaining > 0 {
                attempt.rollback_checking_remaining -= 1;
                json!({"state": "checking", "transition": {"progress": "restoring"}})
            } else {
                attempt.rollback_completed = true;
                json!({"state": "applied"})
            }
        } else {
            return None;
        };
        Some(MockResponse::json(StatusCode::OK, &revision))
    }
}

impl NvueMockHandler for ConfigRevisionRollbackHandler {
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        if request.method == Method::GET && request.uri.path() == ConfigRevisionHandler::CONFIG_PATH
        {
            let query = request.query_pairs();
            if query.len() == 2
                && query
                    .iter()
                    .any(|(key, value)| key == "rev" && value == "applied")
                && query
                    .iter()
                    .any(|(key, value)| key == "filled" && value == "false")
            {
                return None;
            }
        }
        if let Some(captures) = request.route_captures(
            request.method.clone(),
            ConfigRevisionHandler::REVISION_ITEM_ROUTE,
        ) {
            let revision_id = &captures["revision_id"];
            match request.method {
                Method::PATCH => return self.handle_apply(request, revision_id),
                Method::GET if request.uri.query().is_none() && request.body.is_empty() => {
                    if let Some(response) = self.handle_revision_read(revision_id) {
                        return Some(response);
                    }
                }
                _ => {}
            }
        }
        self.storage.handle(request)
    }

    fn verify(&self) -> Vec<String> {
        let state = self.state.lock().expect("rollback state lock should work");
        match state.as_ref() {
            Some(attempt) if attempt.rollback_advertised && !attempt.rollback_completed => {
                vec![format!(
                    "rollback for revision {} did not complete",
                    attempt.revision_id
                )]
            }
            _ => Vec::new(),
        }
    }
}

#[derive(Debug)]
struct RevisionState {
    next_revision: u64,
    applied_revision: String,
    revisions: BTreeMap<String, Revision>,
}

#[derive(Debug)]
struct Revision {
    config: JsonValue,
}

fn unique_query(request: &MockRequest) -> Option<HashMap<String, String>> {
    let mut query = HashMap::new();
    for (key, value) in request.query_pairs() {
        if query.insert(key, value).is_some() {
            return None;
        }
    }
    Some(query)
}

fn single_query_value<'a>(query: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    (query.len() == 1)
        .then(|| query.get(key).map(String::as_str))
        .flatten()
}

fn config_for_revision<'a>(state: &'a RevisionState, revision_id: &str) -> Option<&'a JsonValue> {
    state
        .revisions
        .get(resolve_revision_id(state, revision_id))
        .map(|revision| &revision.config)
}

fn resolve_revision_id<'a>(state: &'a RevisionState, revision_id: &'a str) -> &'a str {
    if revision_id == ConfigRevisionHandler::APPLIED_REVISION {
        state.applied_revision.as_str()
    } else {
        revision_id
    }
}

fn revision_response(revision_id: &str, applied_revision: &str) -> MockResponse {
    let (state, progress) = if revision_id == applied_revision {
        ("applied", "applied")
    } else {
        ("pending", "staged")
    };
    MockResponse::json(
        StatusCode::OK,
        &json!({
            "state": state,
            "transition": {"progress": progress},
        }),
    )
}

fn valid_apply_body(request: &MockRequest) -> bool {
    request.json::<JsonValue>().ok()
        == Some(json!({"state": "apply", "auto-prompt": {"ays": "ays_yes"}}))
}

fn bad_request(message: &str) -> Option<MockResponse> {
    Some(MockResponse::json(
        StatusCode::BAD_REQUEST,
        &json!({"message": message}),
    ))
}

fn not_found(revision_id: &str) -> Option<MockResponse> {
    Some(MockResponse::json(
        StatusCode::NOT_FOUND,
        &json!({"message": format!("unknown revision {revision_id}")}),
    ))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use carbide_test_support::{Check, check_values};

    use super::*;

    fn request(method: Method, uri: &str, body: JsonValue) -> MockRequest {
        MockRequest::new(
            method,
            uri,
            serde_json::to_vec(&body).expect("request JSON should serialize"),
        )
    }

    fn empty_request(method: Method, uri: &str) -> MockRequest {
        MockRequest::new(method, uri, Bytes::new())
    }

    fn response(response: Option<MockResponse>) -> MockResponse {
        response.expect("request should be handled")
    }

    fn response_json(action: Option<MockResponse>) -> JsonValue {
        serde_json::from_slice(&response(action).body).expect("response should contain JSON")
    }

    fn create_revision<H: NvueMockHandler>(handler: &H) -> String {
        let response = response_json(handler.handle(&empty_request(
            Method::POST,
            ConfigRevisionHandler::REVISION_COLLECTION_PATH,
        )));
        response
            .as_object()
            .and_then(|revisions| revisions.keys().next())
            .expect("creation should return one revision")
            .clone()
    }

    #[test]
    fn diff_reports_changed_and_unchanged_staged_configuration() {
        let applied_config = json!({"system": {"hostname": "leaf-1"}});
        let handler = ConfigRevisionHandler::new(applied_config.clone());
        assert_eq!(
            handler.get_revision_config(ConfigRevisionHandler::APPLIED_REVISION),
            Some(applied_config.clone())
        );
        assert_eq!(handler.get_applied_config(), applied_config);

        let revision_id = create_revision(&handler);
        let diff_uri = format!(
            "{}?diff=applied&rev={revision_id}&filled=false",
            ConfigRevisionHandler::CONFIG_PATH
        );

        assert_eq!(
            response_json(handler.handle(&empty_request(Method::GET, &diff_uri))),
            json!({})
        );

        let patch_uri = format!("{}?rev={revision_id}", ConfigRevisionHandler::CONFIG_PATH);
        let changed = json!({"system": {"hostname": "leaf-2"}});
        let patch = response(handler.handle(&request(Method::PATCH, &patch_uri, changed)));
        assert_eq!(patch.status, StatusCode::OK);
        assert_eq!(
            response_json(handler.handle(&empty_request(Method::GET, &diff_uri))),
            json!({"changed": true})
        );
    }

    #[test]
    fn complete_lifecycle_allocates_numeric_revisions_and_applies_immediately() {
        let handler = ConfigRevisionHandler::default();
        let first_revision = create_revision(&handler);
        let second_revision = create_revision(&handler);
        assert_eq!(
            (first_revision.as_str(), second_revision.as_str()),
            ("1", "2")
        );

        let revision_uri = format!(
            "{}?rev={second_revision}",
            ConfigRevisionHandler::CONFIG_PATH
        );
        assert_eq!(
            response(handler.handle(&request(Method::DELETE, &revision_uri, json!({})))).status,
            StatusCode::OK
        );
        let staged_config = json!({"system": {"hostname": "leaf-2"}});
        assert_eq!(
            response(handler.handle(&request(
                Method::PATCH,
                &revision_uri,
                staged_config.clone(),
            )))
            .status,
            StatusCode::OK
        );
        assert_eq!(
            handler.get_revision_config(&second_revision),
            Some(staged_config.clone())
        );

        let diff_uri = format!(
            "{}?diff=applied&rev={second_revision}&filled=false",
            ConfigRevisionHandler::CONFIG_PATH
        );
        assert_eq!(
            response_json(handler.handle(&empty_request(Method::GET, &diff_uri))),
            json!({"changed": true})
        );

        let apply_uri = format!(
            "{}/{second_revision}",
            ConfigRevisionHandler::REVISION_COLLECTION_PATH
        );
        let applied = response_json(handler.handle(&request(
            Method::PATCH,
            &apply_uri,
            json!({"state": "apply", "auto-prompt": {"ays": "ays_yes"}}),
        )));
        assert_eq!(applied["state"], "applied");
        assert_eq!(
            handler.get_revision_config(ConfigRevisionHandler::APPLIED_REVISION),
            handler.get_revision_config(&second_revision)
        );
        assert_eq!(handler.get_applied_config(), staged_config);

        let status = response_json(handler.handle(&empty_request(Method::GET, &apply_uri)));
        assert_eq!(status["state"], "applied");
        assert_eq!(status["transition"]["progress"], "applied");

        let applied_status_uri = format!(
            "{}/{}",
            ConfigRevisionHandler::REVISION_COLLECTION_PATH,
            ConfigRevisionHandler::APPLIED_REVISION,
        );
        let applied_status =
            response_json(handler.handle(&empty_request(Method::GET, &applied_status_uri)));
        assert_eq!(applied_status["state"], "applied");
        assert_eq!(applied_status["transition"]["progress"], "applied");
    }

    #[test]
    fn failed_apply_preserves_applied_config_and_rejects_further_applies() {
        let initial_config = json!({"system": {"hostname": "leaf-0"}});
        let handler = ConfigRevisionRollbackHandler::new(initial_config.clone(), 0, 0);
        let first_revision = create_revision(&handler);
        let second_revision = create_revision(&handler);
        let apply = json!({"state": "apply", "auto-prompt": {"ays": "ays_yes"}});
        let diff_uri = format!("/nvue_v1/?diff=applied&rev={first_revision}&filled=false");
        let staged_config = json!({"system": {"hostname": "leaf-1"}});
        let patch_uri = format!("/nvue_v1/?rev={first_revision}");
        assert_eq!(
            response(handler.handle(&request(Method::PATCH, &patch_uri, staged_config))).status,
            StatusCode::OK
        );

        let revision_uri = format!("/nvue_v1/revision/{first_revision}");
        assert_eq!(
            response(handler.handle(&request(Method::PATCH, &revision_uri, apply.clone()))).status,
            StatusCode::OK
        );
        let second_uri = format!("/nvue_v1/revision/{second_revision}");
        assert_eq!(
            response(handler.handle(&request(Method::PATCH, &second_uri, apply.clone()))).status,
            StatusCode::BAD_REQUEST
        );
        let failed = response_json(handler.handle(&empty_request(Method::GET, &revision_uri)));
        assert_eq!(failed["state"], "apply_fail");
        let rollback_id = failed["transition"]["issue"]["2"]["data"]["rollback_target"]
            .as_str()
            .expect("failure should advertise a rollback revision");
        assert!(!handler.verify().is_empty());
        let rollback_uri = format!("/nvue_v1/revision/{}", urlencoding::encode(rollback_id));
        assert_eq!(
            response_json(handler.handle(&empty_request(Method::GET, &rollback_uri)))["state"],
            "applied"
        );
        assert_eq!(handler.get_applied_config(), initial_config);
        assert_eq!(
            response_json(handler.handle(&empty_request(Method::GET, &diff_uri))),
            json!({"changed": true})
        );
        assert_eq!(
            response(handler.handle(&request(Method::PATCH, &second_uri, apply))).status,
            StatusCode::BAD_REQUEST
        );
        assert!(handler.verify().is_empty());
    }

    #[tokio::test]
    async fn applied_config_http_read_is_unhandled_but_diff_is_supported() {
        let handler = ConfigRevisionRollbackHandler::new(json!({}), 0, 0);
        let server =
            super::super::super::MockNvueServer::start(handler).expect("mock server should start");
        let client =
            crate::NvueClient::new(server.server_address()).expect("client should be created");
        let revision_id = client
            .create_config_revision()
            .await
            .expect("revision creation should succeed");
        assert!(
            client
                .get_revision_config_diff("applied", &revision_id)
                .await
                .expect("diff against applied should succeed")
                .is_empty()
        );
        assert!(client.get_applied_config().await.is_err());
        let failure = server
            .finish()
            .await
            .expect_err("applied read must be unhandled");
        assert!(
            failure.contains("unhandled request: GET /nvue_v1/?rev=applied&filled=false"),
            "{failure}"
        );
    }

    #[test]
    fn malformed_owned_requests_return_bad_request() {
        let handler = ConfigRevisionHandler::default();
        let revision_id = create_revision(&handler);
        let revision_uri = format!(
            "{}/{revision_id}",
            ConfigRevisionHandler::REVISION_COLLECTION_PATH
        );

        check_values(
            [
                Check {
                    scenario: "revision creation uses the wrong method",
                    input: empty_request(
                        Method::GET,
                        ConfigRevisionHandler::REVISION_COLLECTION_PATH,
                    ),
                    expect: StatusCode::BAD_REQUEST,
                },
                Check {
                    scenario: "revision creation includes a body",
                    input: request(
                        Method::POST,
                        ConfigRevisionHandler::REVISION_COLLECTION_PATH,
                        json!({}),
                    ),
                    expect: StatusCode::BAD_REQUEST,
                },
                Check {
                    scenario: "staged config omits the revision",
                    input: request(Method::PATCH, ConfigRevisionHandler::CONFIG_PATH, json!({})),
                    expect: StatusCode::BAD_REQUEST,
                },
                Check {
                    scenario: "staged config is not an object",
                    input: request(
                        Method::PATCH,
                        &format!("{}?rev={revision_id}", ConfigRevisionHandler::CONFIG_PATH),
                        json!([]),
                    ),
                    expect: StatusCode::BAD_REQUEST,
                },
                Check {
                    scenario: "apply body is incomplete",
                    input: request(Method::PATCH, &revision_uri, json!({"state": "apply"})),
                    expect: StatusCode::BAD_REQUEST,
                },
            ],
            |request| response(handler.handle(&request)).status,
        );
    }

    #[test]
    fn unknown_revisions_return_not_found() {
        let handler = ConfigRevisionHandler::default();

        check_values(
            [
                Check {
                    scenario: "unknown staged revision",
                    input: request(
                        Method::PATCH,
                        &format!("{}?rev=404", ConfigRevisionHandler::CONFIG_PATH),
                        json!({}),
                    ),
                    expect: StatusCode::NOT_FOUND,
                },
                Check {
                    scenario: "unknown revision status",
                    input: empty_request(Method::GET, "/nvue_v1/revision/404"),
                    expect: StatusCode::NOT_FOUND,
                },
                Check {
                    scenario: "unknown diff target",
                    input: empty_request(
                        Method::GET,
                        &format!(
                            "{}?diff=applied&rev=404&filled=false",
                            ConfigRevisionHandler::CONFIG_PATH
                        ),
                    ),
                    expect: StatusCode::NOT_FOUND,
                },
            ],
            |request| response(handler.handle(&request)).status,
        );
    }

    #[test]
    fn unrelated_routes_are_unhandled() {
        let handler = ConfigRevisionHandler::default();
        let request = empty_request(Method::GET, "/nvue_v1/system");

        assert_eq!(handler.handle(&request), None);
    }
}
