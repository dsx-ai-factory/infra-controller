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

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use glob::Pattern;
use rand::RngExt;
use serde_json::Value;

use super::{Action, Rule, RuleId, Selector};
use crate::is_json_response;

#[derive(Debug, Default)]
pub struct InjectionStore {
    rules: ArcSwap<Vec<Arc<Rule>>>,
}

impl InjectionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self) -> Vec<Arc<Rule>> {
        (*self.rules.load_full()).clone()
    }

    /// Replace the entire rule set.
    pub fn put(&self, rules: Vec<Rule>) {
        let new_rules: Vec<Arc<Rule>> = rules.into_iter().map(Arc::new).collect();
        self.rules.store(Arc::new(new_rules));
    }

    /// Insert or replace a single rule
    pub fn upsert(&self, rule: Rule) -> RuleId {
        let id = rule.id.clone();
        let new_rule = Arc::new(rule);
        self.rules.rcu(|current| {
            let mut next: Vec<Arc<Rule>> = (**current).clone();
            if let Some(slot) = next.iter_mut().find(|r| r.id == id) {
                *slot = Arc::clone(&new_rule);
            } else {
                next.push(Arc::clone(&new_rule));
            }
            Arc::new(next)
        });
        id
    }

    /// Remove a rule by id
    pub fn delete(&self, id: &RuleId) -> bool {
        self.rcu_with::<bool>(|current| {
            if !current.iter().any(|r| &r.id == id) {
                return (Arc::clone(current), false);
            }
            let next: Vec<Arc<Rule>> = current.iter().filter(|r| &r.id != id).cloned().collect();
            (Arc::new(next), true)
        })
    }

    pub fn clear(&self) {
        self.rules.store(Arc::new(Vec::new()));
    }

    fn rcu_with<R: Default>(
        &self,
        mut f: impl FnMut(&Arc<Vec<Arc<Rule>>>) -> (Arc<Vec<Arc<Rule>>>, R),
    ) -> R {
        let out: Cell<R> = Cell::new(R::default());
        self.rules.rcu(|current| {
            let (next, aux) = f(current);
            out.set(aux);
            next
        });
        out.into_inner()
    }

    /// Used to modify HTTP response and inject latency.
    pub async fn pre_handle(&self, method: &Method, path: &str) -> Option<Response> {
        let snapshot = self.rules.load_full();
        if snapshot.is_empty() {
            return None;
        }

        let method_str = method.as_str();
        let mut total_delay = Duration::ZERO;
        let mut status_inject: Option<StatusCode> = None;

        for rule in snapshot.iter() {
            if !selector_matches(&rule.selector, Some(method_str), path) {
                continue;
            }
            match &rule.action {
                Action::Status(code) => {
                    // Inject only the first matching status
                    if status_inject.is_some() {
                        continue;
                    }
                    if !self.try_consume(&rule.id) {
                        continue;
                    }
                    let status =
                        StatusCode::from_u16(*code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    tracing::info!(
                        method = method_str,
                        path,
                        rule = %rule.id,
                        http_status = %status,
                        "injecting status",
                    );
                    status_inject = Some(status);
                }
                Action::Latency { mean, jitter } => {
                    if !self.try_consume(&rule.id) {
                        continue;
                    }
                    let extra = if jitter.is_zero() {
                        Duration::ZERO
                    } else {
                        let bound = jitter.as_micros().max(1) as u64;
                        Duration::from_micros(rand::rng().random_range(0..bound))
                    };
                    let delay = *mean + extra;
                    tracing::info!(
                        method = method_str,
                        path,
                        rule = %rule.id,
                        delay_milliseconds = delay.as_millis() as u64,
                        "injecting latency",
                    );
                    // Combine delay if multiple rules match.
                    total_delay = total_delay.saturating_add(delay);
                }
                _ => {}
            }
        }

        if !total_delay.is_zero() {
            tokio::time::sleep(total_delay).await;
        }

        status_inject.map(|status| {
            let mut resp = status.into_response();
            *resp.body_mut() = Body::empty();
            resp
        })
    }

    /// Modify response body
    pub async fn post_handle(&self, path: &str, response: Response) -> Response {
        // Only eligible JSON responses consume body-mutation rules. In
        // particular, never buffer an SSE body or spend its finite rules.
        if !response.status().is_success() || !is_json_response(&response) {
            return response;
        }
        let snapshot = self.rules.load_full();
        if snapshot.is_empty() {
            return response;
        }

        let mut replace: Option<(&RuleId, &Value)> = None;
        let mut merges: Vec<(&RuleId, &Value)> = Vec::new();

        for rule in snapshot.iter() {
            match &rule.action {
                Action::Replace(value)
                    if replace.is_none()
                        && selector_matches(&rule.selector, None, path)
                        && self.try_consume(&rule.id) =>
                {
                    replace = Some((&rule.id, value));
                }
                Action::JsonMerge(patch)
                    if selector_matches(&rule.selector, None, path)
                        && self.try_consume(&rule.id) =>
                {
                    merges.push((&rule.id, patch));
                }
                _ => {}
            }
        }

        if replace.is_none() && merges.is_empty() {
            return response;
        }

        let (parts, body) = response.into_parts();

        let mut json: Value = if let Some((id, value)) = replace {
            tracing::info!(path, rule = %id, "injection: replacing body");
            value.clone()
        } else {
            let bytes = match axum::body::to_bytes(body, usize::MAX).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(path, error = ?e, "injection: failed to buffer response body");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("injection: could not buffer inner response body: {e}"),
                    )
                        .into_response();
                }
            };
            match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        path,
                        error = ?e,
                        "injection: inner response not JSON; returning as-is",
                    );
                    return (parts, bytes).into_response();
                }
            }
        };

        for (id, patch) in &merges {
            tracing::info!(path, rule = %id, "injection: applying JsonMerge");
            json_patch(&mut json, (*patch).clone());
        }

        let new_bytes = match serde_json::to_vec(&json) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path, error = ?e, "injection: failed to re-serialize body");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("injection: could not re-serialize injected body: {e}"),
                )
                    .into_response();
            }
        };

        let mut resp = (parts, Body::from(new_bytes)).into_response();
        // Body length has changed; let hyper recompute Content-Length.
        resp.headers_mut()
            .remove(axum::http::header::CONTENT_LENGTH);
        resp
    }

    /// Consume rule, if remaing <= 1, then delete it from the store
    fn try_consume(&self, rule_id: &RuleId) -> bool {
        self.rcu_with::<bool>(|current| {
            let Some(idx) = current.iter().position(|r| &r.id == rule_id) else {
                return (Arc::clone(current), false);
            };
            let rule = &current[idx];
            match rule.remaining {
                None => (Arc::clone(current), true),
                Some(0) => {
                    let mut next = (**current).clone();
                    next.remove(idx);
                    (Arc::new(next), false)
                }
                Some(1) => {
                    let mut next = (**current).clone();
                    next.remove(idx);
                    (Arc::new(next), true)
                }
                Some(n) => {
                    let mut next = (**current).clone();
                    let mut new_rule = (**rule).clone();
                    new_rule.remaining = Some(n - 1);
                    next[idx] = Arc::new(new_rule);
                    (Arc::new(next), true)
                }
            }
        })
    }
}

fn json_patch(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target_object), Value::Object(patch_object)) => {
            for (key, patch_value) in patch_object {
                match target_object.get_mut(&key) {
                    Some(target_value) => json_patch(target_value, patch_value),
                    None => {
                        target_object.insert(key, patch_value);
                    }
                }
            }
        }
        (target_slot, patch_value) => *target_slot = patch_value,
    }
}

fn selector_matches(selector: &Selector, method: Option<&str>, path: &str) -> bool {
    match selector {
        Selector::OdataId(glob) => match Pattern::new(glob) {
            Ok(p) => p.matches(path),
            Err(e) => {
                tracing::warn!(glob, error = ?e, "injection: invalid OdataId glob");
                false
            }
        },
        Selector::Path { method: m, glob } => {
            if let (Some(want), Some(got)) = (m.as_deref(), method)
                && !want.eq_ignore_ascii_case(got)
            {
                return false;
            }
            match Pattern::new(glob) {
                Ok(p) => p.matches(path),
                Err(e) => {
                    tracing::warn!(glob, error = ?e, "injection: invalid Path glob");
                    false
                }
            }
        }
        Selector::Any(children) => children.iter().any(|c| selector_matches(c, method, path)),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::header;
    use serde_json::json;

    use super::*;

    fn json_ok(value: Value) -> Response {
        let body = value.to_string();
        let mut resp = Response::new(Body::from(body));
        *resp.status_mut() = StatusCode::OK;
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        resp
    }

    async fn body_json(resp: Response) -> Value {
        let (_, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(bytes.as_ref()).unwrap()
    }

    fn rule(id: &str, selector: Selector, action: Action) -> Rule {
        Rule {
            id: id.into(),
            selector,
            action,
            remaining: None,
        }
    }

    #[tokio::test]
    async fn json_merge_deep_merges_into_response() {
        let store = InjectionStore::new();
        store.put(vec![rule(
            "warm",
            Selector::OdataId("/redfish/v1/Chassis/X/Sensors/Temp_1".into()),
            Action::JsonMerge(json!({
                "Reading": 95.0,
                "Status": { "Health": "Critical" }
            })),
        )]);

        let inner = json_ok(json!({
            "Id": "Temp_1",
            "Reading": 30.0,
            "Status": { "Health": "OK", "State": "Enabled" },
            "@odata.id": "/redfish/v1/Chassis/X/Sensors/Temp_1",
        }));
        let out = store
            .post_handle("/redfish/v1/Chassis/X/Sensors/Temp_1", inner)
            .await;
        let body = body_json(out).await;
        assert_eq!(body["Id"], json!("Temp_1"));
        assert_eq!(body["Reading"], json!(95.0));
        assert_eq!(body["Status"]["Health"], json!("Critical"));
        assert_eq!(
            body["Status"]["State"],
            json!("Enabled"),
            "untouched sibling fields must survive deep merge"
        );
    }

    #[tokio::test]
    async fn replace_short_circuits_inner_body_but_merges_still_compose() {
        let store = InjectionStore::new();
        store.put(vec![
            rule(
                "replace",
                Selector::OdataId("/r".into()),
                Action::Replace(json!({ "from": "replace" })),
            ),
            rule(
                "merge",
                Selector::OdataId("/r".into()),
                Action::JsonMerge(json!({ "added": true })),
            ),
        ]);

        let inner = json_ok(json!({ "from": "inner", "leftover": 42 }));
        let body = body_json(store.post_handle("/r", inner).await).await;
        assert_eq!(body["from"], json!("replace"));
        assert_eq!(body["added"], json!(true));
        assert!(body.get("leftover").is_none(), "inner body must be dropped");
    }

    #[tokio::test]
    async fn status_short_circuits_inner_handler() {
        let store = InjectionStore::new();
        store.put(vec![Rule {
            id: "boom".into(),
            selector: Selector::Path {
                method: Some("GET".into()),
                glob: "/x".into(),
            },
            action: Action::Status(503),
            remaining: Some(2),
        }]);

        let r1 = store.pre_handle(&Method::GET, "/x").await;
        assert_eq!(r1.expect("first call short-circuits").status(), 503);
        let r2 = store.pre_handle(&Method::GET, "/x").await;
        assert_eq!(r2.expect("second call short-circuits").status(), 503);
        let r3 = store.pre_handle(&Method::GET, "/x").await;
        assert!(
            r3.is_none(),
            "third call must NOT short-circuit (counter exhausted)"
        );

        // Method mismatch must not match.
        store.put(vec![Rule {
            id: "boom".into(),
            selector: Selector::Path {
                method: Some("POST".into()),
                glob: "/x".into(),
            },
            action: Action::Status(418),
            remaining: None,
        }]);
        assert!(store.pre_handle(&Method::GET, "/x").await.is_none());
        assert_eq!(
            store
                .pre_handle(&Method::POST, "/x")
                .await
                .expect("post match")
                .status(),
            418
        );
    }

    #[tokio::test(start_paused = true)]
    async fn latency_action_delays_pre_handle() {
        let store = InjectionStore::new();
        store.put(vec![rule(
            "slow",
            Selector::Path {
                method: None,
                glob: "/slow".into(),
            },
            Action::Latency {
                mean: Duration::from_secs(2),
                jitter: Duration::ZERO,
            },
        )]);

        let start = tokio::time::Instant::now();
        let r = store.pre_handle(&Method::GET, "/slow").await;
        let elapsed = start.elapsed();
        assert!(r.is_none(), "latency rules must not short-circuit");
        assert!(
            elapsed >= Duration::from_secs(2),
            "expected >=2s delay, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn odata_id_glob_fans_out_across_resources() {
        let store = InjectionStore::new();
        store.put(vec![rule(
            "hot",
            Selector::OdataId("/redfish/v1/Chassis/*/Sensors/Temp_*".into()),
            Action::JsonMerge(json!({ "Reading": 99.0 })),
        )]);

        for path in [
            "/redfish/v1/Chassis/A/Sensors/Temp_1",
            "/redfish/v1/Chassis/B/Sensors/Temp_42",
        ] {
            let body = body_json(
                store
                    .post_handle(path, json_ok(json!({ "Reading": 25.0 })))
                    .await,
            )
            .await;
            assert_eq!(body["Reading"], json!(99.0), "{path} must match the glob");
        }

        let untouched = body_json(
            store
                .post_handle(
                    "/redfish/v1/Chassis/A/Sensors/Volt_1",
                    json_ok(json!({ "Reading": 25.0 })),
                )
                .await,
        )
        .await;
        assert_eq!(
            untouched["Reading"],
            json!(25.0),
            "non-Temp sensor must NOT match"
        );
    }

    #[tokio::test]
    async fn counter_exhaustion_removes_rule_from_store() {
        let store = InjectionStore::new();
        store.put(vec![Rule {
            id: "limited".into(),
            selector: Selector::OdataId("/r".into()),
            action: Action::JsonMerge(json!({ "n": 1 })),
            remaining: Some(2),
        }]);

        let body = body_json(store.post_handle("/r", json_ok(json!({ "n": 0 }))).await).await;
        assert_eq!(body["n"], json!(1), "first match fires");
        assert_eq!(
            store.list()[0].remaining,
            Some(1),
            "remaining decremented after first fire"
        );

        let body = body_json(store.post_handle("/r", json_ok(json!({ "n": 0 }))).await).await;
        assert_eq!(body["n"], json!(1), "last fire is still applied");
        assert!(
            store.list().is_empty(),
            "rule must be removed after its budget is consumed"
        );

        let body = body_json(store.post_handle("/r", json_ok(json!({ "n": 0 }))).await).await;
        assert_eq!(body["n"], json!(0), "no further fires after exhaustion");
    }

    #[tokio::test]
    async fn delete_and_clear_admin_methods_work() {
        let store = InjectionStore::new();
        store.put(vec![
            rule("a", Selector::OdataId("/a".into()), Action::Status(404)),
            rule("b", Selector::OdataId("/b".into()), Action::Status(404)),
        ]);
        assert_eq!(store.list().len(), 2);
        assert!(store.delete(&"a".into()));
        assert!(!store.delete(&"a".into()));
        assert_eq!(store.list().len(), 1);
        store.clear();
        assert!(store.list().is_empty());
    }

    #[test]
    fn rules_serialize_round_trip() {
        let rules = vec![
            rule(
                "hide",
                Selector::OdataId("/resources/hidden/*".into()),
                Action::Status(404),
            ),
            rule(
                "merge",
                Selector::OdataId("/resources/*".into()),
                Action::JsonMerge(json!({ "Status": { "Health": "Critical" } })),
            ),
        ];
        let serialized = serde_json::to_value(&rules).unwrap();
        let deserialized: Vec<Rule> = serde_json::from_value(serialized.clone()).unwrap();

        assert_eq!(serde_json::to_value(deserialized).unwrap(), serialized);
    }

    #[tokio::test]
    async fn replace_does_not_require_valid_inner_json() {
        // The Replace fast path must not buffer or parse the upstream body —
        // hand it garbage that claims to be JSON and prove the Replace payload
        // still comes out intact.
        let store = InjectionStore::new();
        store.put(vec![rule(
            "replace",
            Selector::OdataId("/r".into()),
            Action::Replace(json!({ "from": "replace" })),
        )]);

        let mut bad = Response::new(Body::from("this is not { json"));
        *bad.status_mut() = StatusCode::OK;
        bad.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        let out = store.post_handle("/r", bad).await;
        assert_eq!(out.status(), StatusCode::OK);
        let body = body_json(out).await;
        assert_eq!(body, json!({ "from": "replace" }));
    }

    #[tokio::test]
    async fn error_responses_are_not_mutated() {
        // A non-2xx response must pass through untouched even when rules match
        // its path — otherwise the original error semantics would be silently
        // overwritten by injected JSON.
        let store = InjectionStore::new();
        store.put(vec![Rule {
            id: "merge".into(),
            selector: Selector::OdataId("/r".into()),
            action: Action::JsonMerge(json!({ "added": true })),
            remaining: Some(1),
        }]);

        let mut err = Response::new(Body::from(r#"{"error":"oops"}"#));
        *err.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        err.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        let out = store.post_handle("/r", err).await;
        assert_eq!(out.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(out).await;
        assert_eq!(body, json!({ "error": "oops" }));
        assert_eq!(store.list()[0].remaining, Some(1));
    }

    #[tokio::test]
    async fn sse_responses_preserve_finite_json_rule_budget() {
        let store = InjectionStore::new();
        store.put(vec![Rule {
            id: "once".into(),
            selector: Selector::OdataId("/r".into()),
            action: Action::Replace(json!({"replaced": true})),
            remaining: Some(1),
        }]);
        let response = ([("content-type", "text/event-stream")], "data: {}\n\n").into_response();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.post_handle("/r", response),
        )
        .await
        .expect("stream must not be buffered");
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(bytes, "data: {}\n\n");
        assert_eq!(store.list()[0].remaining, Some(1));
        let json = body_json(store.post_handle("/r", json_ok(json!({}))).await).await;
        assert_eq!(json, json!({"replaced": true}));
        assert!(store.list().is_empty());
    }

    #[tokio::test]
    async fn non_json_response_passes_through_unchanged() {
        let store = InjectionStore::new();
        store.put(vec![rule(
            "merge",
            Selector::OdataId("/r".into()),
            Action::JsonMerge(json!({ "added": true })),
        )]);

        let mut resp = "plain body".into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/plain"),
        );
        let out = store.post_handle("/r", resp).await;
        let (parts, body) = out.into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(bytes.as_ref(), b"plain body");
    }
}
