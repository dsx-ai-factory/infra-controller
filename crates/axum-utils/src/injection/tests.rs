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

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{Action, InjectionStore, Rule, Selector, injection_router, management_router};

#[tokio::test]
async fn management_router_updates_the_shared_store() {
    let store = Arc::new(InjectionStore::new());
    let router = management_router(Arc::clone(&store));
    let rule = json!({
        "id": "unavailable",
        "selector": { "Path": { "method": "GET", "glob": "/resource" } },
        "action": { "Status": 503 }
    });

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/Injection/rules")
                .header("content-type", "application/json")
                .body(Body::from(rule.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.list().len(), 1);

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/Injection/rules/unavailable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(store.list().is_empty());
}

#[tokio::test]
async fn injection_router_applies_store_rules() {
    let store = Arc::new(InjectionStore::new());
    store.put(vec![Rule {
        id: "merge".into(),
        selector: Selector::Path {
            method: Some("GET".into()),
            glob: "/resource".into(),
        },
        action: Action::JsonMerge(json!({ "injected": true })),
        remaining: None,
    }]);
    let router = injection_router(
        Router::new().route(
            "/resource",
            get(|| async { axum::Json(json!({ "inner": true })) }),
        ),
        store,
    );

    let response = router
        .oneshot(
            Request::builder()
                .uri("/resource")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body, json!({ "inner": true, "injected": true }));
}
