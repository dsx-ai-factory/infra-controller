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

//! Generic HTTP fault- and value-injection store.
//!
//! The store holds an ordered list of [`Rule`]s. Each rule pairs a [`Selector`]
//! (matching an odata id or arbitrary path) with an [`Action`] (mutate the
//! response body, replace it wholesale, slow it down, or short-circuit
//! it with a status code).
//!
//! At HTTP request time the middleware calls `InjectionStore::pre_handle`
//! before invoking the inner router; if that returns `Some(response)`
//! the inner handler is skipped (`Status` short-circuit) or its execution is
//! delayed (`Latency`). After the inner handler runs the middleware calls
//! `InjectionStore::post_handle` to apply `Replace` / `JsonMerge` actions
//! on successful JSON responses. Non-2xx and non-JSON responses pass through
//! without consuming finite Replace/JsonMerge rule budgets.
//!
//! Globbing follows the [`glob`] crate (filesystem-style):
//! - `*` matches any sequence of characters except `/`
//! - `**` matches across `/` separators
//! - `?` matches any single character
//!
//! Each rule's optional `remaining` field bounds how many times it may fire;
//! when a bounded rule's last fire is consumed the rule is removed from the
//! store. All consumption is atomic via [`arc_swap::ArcSwap`] so concurrent
//! requests against the same rule never over-fire.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
pub use middleware::injection_router;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use store::InjectionStore;

mod middleware;
mod store;

#[cfg(test)]
mod tests;

/// Adds management endpoints for an [`InjectionStore`].
pub fn management_router(store: Arc<InjectionStore>) -> Router {
    Router::new()
        .route(
            "/Injection/rules",
            get(list_rules)
                .put(put_rules)
                .post(upsert_rule)
                .delete(clear_rules),
        )
        .route("/Injection/rules/{id}", delete(delete_rule))
        .with_state(store)
}

async fn list_rules(State(store): State<Arc<InjectionStore>>) -> Response {
    rules_response(&store)
}

async fn put_rules(
    State(store): State<Arc<InjectionStore>>,
    Json(rules): Json<Vec<Rule>>,
) -> Response {
    store.put(rules);
    rules_response(&store)
}

async fn upsert_rule(State(store): State<Arc<InjectionStore>>, Json(rule): Json<Rule>) -> Response {
    store.upsert(rule);
    rules_response(&store)
}

async fn delete_rule(State(store): State<Arc<InjectionStore>>, Path(id): Path<String>) -> Response {
    if store.delete(&RuleId::from(id)) {
        rules_response(&store)
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn clear_rules(State(store): State<Arc<InjectionStore>>) -> Response {
    store.clear();
    rules_response(&store)
}

fn rules_response(store: &InjectionStore) -> Response {
    let snapshot = store.list();
    let owned: Vec<&Rule> = snapshot.iter().map(Arc::as_ref).collect();
    match serde_json::to_value(owned) {
        Ok(v) => Json(v).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{err:?}")})),
        )
            .into_response(),
    }
}

#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleId(String);

impl From<&str> for RuleId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for RuleId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for RuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub selector: Selector,
    pub action: Action,
    /// Maximum number of times this rule may fire.
    ///
    /// `None` means unlimited. When a bounded counter reaches zero the rule
    /// is automatically removed from the store on the next consume attempt.
    #[serde(default)]
    pub remaining: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Selector {
    /// Glob against the Redfish `@odata.id`
    /// Example: `"/redfish/v1/Chassis/*/Sensors/Temp_*"`
    OdataId(String),
    /// Glob but with HTTP method
    Path {
        #[serde(default)]
        method: Option<String>,
        glob: String,
    },
    /// Support for nested match, OR clause
    Any(Vec<Selector>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Action {
    JsonMerge(Value),
    Replace(Value),
    /// Replace status code
    Status(u16),
    Latency {
        #[serde(with = "duration_str_serde")]
        mean: Duration,
        #[serde(with = "duration_str_serde", default)]
        jitter: Duration,
    },
}

mod duration_str_serde {
    use std::time::Duration;

    use serde::{Deserializer, Serializer};

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        duration_str::deserialize_duration(d)
    }

    pub(super) fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{}ms", d.as_millis()))
    }
}
