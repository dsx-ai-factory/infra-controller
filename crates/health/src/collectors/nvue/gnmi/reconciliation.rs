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

//! Presence-only reconciliation for retained gNMI metric readings.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::proto::{self, PathElem};
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

type MetricKey = (String, String, String);
type SourcePath = Vec<(String, BTreeMap<String, String>)>;

pub(super) type SharedMetricSources =
    Arc<Mutex<HashMap<MetricKey, (SourcePath, Option<MetricSample>)>>>;

/// Retains source ownership and protects readings touched during a snapshot.
pub(super) struct MetricReconciler {
    sources: SharedMetricSources,
    eligible: Mutex<HashMap<SourcePath, HashSet<MetricKey>>>,
    retain_samples: bool,
    sink: Option<Arc<dyn DataSink>>,
    context: EventContext,
}

impl MetricReconciler {
    pub(super) fn new(
        sink: Option<Arc<dyn DataSink>>,
        context: EventContext,
        sources: Option<SharedMetricSources>,
    ) -> Self {
        Self {
            retain_samples: sources.is_some(),
            sources: sources.unwrap_or_default(),
            eligible: Mutex::new(HashMap::new()),
            sink,
            context,
        }
    }

    /// Publishes retained readings with this collector's current endpoint metadata.
    pub(super) fn republish(&self) {
        let Some(sink) = &self.sink else {
            return;
        };

        let sources = self
            .sources
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        for (_, sample) in sources.values() {
            if let Some(sample) = sample {
                sink.handle_event(
                    &self.context,
                    &CollectorEvent::Metric(Box::new(sample.clone())),
                );
            }
        }
    }

    pub(super) fn record(&self, sample: &MetricSample, path: &[&PathElem]) {
        let key = (
            sample.key.clone(),
            sample.metric_type.clone(),
            sample.unit.clone(),
        );

        let mut sources = self
            .sources
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let path = source_path(path.iter().copied());

        self.eligible
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&path);

        sources.insert(key, (path, self.retain_samples.then(|| sample.clone())));
    }

    /// A live update protects its source even if the new value cannot be projected.
    pub(super) fn touch(&self, path: &[&PathElem]) {
        let mut eligible = self
            .eligible
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        if !eligible.is_empty() {
            eligible.remove(&source_path(path.iter().copied()));
        }
    }

    /// Applies live Deletes using the source that last wrote each metric key.
    pub(super) fn delete(&self, path: &[&PathElem]) {
        let deleted = source_path(path.iter().copied());

        let mut sources = self
            .sources
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        sources.retain(|(key, kind, unit), (source, _)| {
            if !covers(&deleted, source) {
                return true;
            }

            if let Some(sink) = &self.sink {
                sink.prune_metric_key(&self.context, key, kind, unit);
            }

            false
        });
    }

    pub(super) fn begin(&self, request: &proto::SubscribeRequest) -> Option<MetricSnapshot> {
        let Some(proto::subscribe_request::Request::Subscribe(list)) = &request.request else {
            return None;
        };

        let sources = self
            .sources
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        if sources.is_empty() {
            return None;
        }

        let mut eligible: HashMap<SourcePath, HashSet<MetricKey>> = HashMap::new();

        for (key, (source, _)) in sources.iter() {
            eligible
                .entry(source.clone())
                .or_default()
                .insert(key.clone());
        }

        *self
            .eligible
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = eligible;

        let prefix = list.prefix.clone().unwrap_or_default();

        let requested = list
            .subscription
            .iter()
            .filter_map(|subscription| {
                subscription
                    .path
                    .as_ref()
                    .map(|path| source_path(prefix.elem.iter().chain(&path.elem)))
            })
            .collect();

        let mut key_schemas = HashMap::new();
        let mut leaf_paths = HashSet::new();

        for (source, _) in sources.values() {
            let mut names = Vec::new();

            for (name, keys) in source {
                names.push(name.clone());
                key_schemas.insert(names.clone(), keys.keys().cloned().collect::<Vec<_>>());
            }

            leaf_paths.insert(names);
        }

        Some(MetricSnapshot {
            candidates: sources.values().map(|(source, _)| source.clone()).collect(),
            present: HashSet::new(),
            requested,
            target: prefix.target,
            origin: prefix.origin,
            key_schemas,
            leaf_paths,
        })
    }

    /// A failed or cancelled snapshot discards eligibility without pruning.
    pub(super) fn finish(&self, snapshot: Option<MetricSnapshot>) -> usize {
        let mut sources = self
            .sources
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let eligible = std::mem::take(
            &mut *self
                .eligible
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );

        let Some(snapshot) = snapshot else {
            return 0;
        };

        let mut removed = 0;

        sources.retain(|(key, kind, unit), (source, _)| {
            if !eligible
                .get(source)
                .is_some_and(|keys| keys.contains(&(key.clone(), kind.clone(), unit.clone())))
                || snapshot.present.contains(source)
            {
                return true;
            }

            if let Some(sink) = &self.sink {
                sink.prune_metric_key(&self.context, key, kind, unit);
            }

            removed += 1;

            false
        });

        removed
    }
}

/// Reads path presence without replaying values through metric or log sinks.
/// Mapped leaves may omit `val` or leave its inner value unset.
/// Populated values on mapped leaves must be scalar.
pub(super) struct MetricSnapshot {
    candidates: HashSet<SourcePath>,
    present: HashSet<SourcePath>,
    requested: Vec<SourcePath>,
    target: String,
    origin: String,
    key_schemas: HashMap<Vec<String>, Vec<String>>,
    leaf_paths: HashSet<Vec<String>>,
}

impl MetricSnapshot {
    #[allow(deprecated)]
    pub(super) fn process_response(
        &mut self,
        response: &proto::SubscribeResponse,
    ) -> Result<bool, tonic::Status> {
        let notification = match &response.response {
            Some(proto::subscribe_response::Response::SyncResponse(complete)) => {
                return Ok(*complete);
            }
            Some(proto::subscribe_response::Response::Update(notification)) => notification,
            _ => {
                return Err(tonic::Status::invalid_argument(
                    "unexpected metric snapshot response",
                ));
            }
        };

        let prefix = notification.prefix.as_ref();

        let paths = prefix
            .into_iter()
            .chain(
                notification
                    .update
                    .iter()
                    .filter_map(|update| update.path.as_ref()),
            )
            .chain(notification.delete.iter());

        if paths.into_iter().any(|path| {
            !path.element.is_empty()
                || (!path.target.is_empty() && path.target != self.target)
                || !snapshot_origin_matches(&self.origin, &path.origin)
                || path
                    .elem
                    .iter()
                    .any(|elem| elem.name.is_empty() || elem.key.values().any(String::is_empty))
        }) {
            return Err(tonic::Status::invalid_argument(
                "unsupported metric snapshot path",
            ));
        }

        let prefix = prefix.map(|path| path.elem.as_slice()).unwrap_or_default();

        for deleted in &notification.delete {
            let path = source_path(prefix.iter().chain(&deleted.elem));

            self.validate_path(&path, true)?;
            self.present.retain(|source| !covers(&path, source));
        }

        for update in &notification.update {
            let path = update
                .path
                .as_ref()
                .ok_or_else(|| tonic::Status::invalid_argument("missing metric snapshot path"))?;

            let path = source_path(prefix.iter().chain(&path.elem));

            let mapped = self.validate_path(&path, false)?;

            if mapped
                && update
                    .val
                    .as_ref()
                    .is_some_and(|value| value.value.is_some())
                && !scalar_value(update.val.as_ref())
            {
                return Err(tonic::Status::invalid_argument(
                    "unsupported metric snapshot value",
                ));
            }

            if self.candidates.contains(&path) {
                self.present.insert(path);
            }
        }

        Ok(false)
    }

    /// Rejects aggregates and changed list-key schemas instead of inferring absence.
    fn validate_path(&self, path: &SourcePath, delete: bool) -> Result<bool, tonic::Status> {
        if !self
            .requested
            .iter()
            .any(|requested| covers(requested, path) || (delete && covers(path, requested)))
        {
            return Err(tonic::Status::invalid_argument(
                "metric snapshot path outside subscription",
            ));
        }

        let mut names = Vec::new();

        for (name, keys) in path {
            names.push(name.clone());

            // Ancestor Deletes may omit keys to remove an entire list.
            if let Some(expected) = self.key_schemas.get(&names)
                && !(delete && keys.is_empty())
                && keys.keys().ne(expected.iter())
            {
                return Err(tonic::Status::invalid_argument(
                    "unsupported metric snapshot keys",
                ));
            }
        }

        let mapped = self.leaf_paths.contains(&names);

        if !delete && !mapped && self.key_schemas.contains_key(&names) {
            return Err(tonic::Status::invalid_argument(
                "aggregate metric snapshot path",
            ));
        }

        Ok(mapped)
    }
}

/// Omitted response origins inherit the subscription scope; an omitted request
/// origin defaults to `openconfig` under the gNMI origin contract.
pub(super) fn snapshot_origin_matches(requested: &str, response: &str) -> bool {
    response.is_empty()
        || response == requested
        || (requested.is_empty() && response == "openconfig")
}

fn source_path<'a>(elements: impl Iterator<Item = &'a PathElem>) -> SourcePath {
    elements
        .map(|elem| {
            (
                elem.name.clone(),
                elem.key
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        })
        .collect()
}

fn covers(ancestor: &SourcePath, path: &SourcePath) -> bool {
    ancestor.len() <= path.len()
        && ancestor.iter().zip(path).all(|(expected, actual)| {
            expected.0 == actual.0
                && expected
                    .1
                    .iter()
                    .all(|(key, value)| actual.1.get(key) == Some(value))
        })
}

/// Accepts scalar snapshot values while rejecting aggregates and missing values.
#[allow(deprecated, reason = "accept legacy gNMI scalar FloatVal responses")]
pub(super) fn scalar_value(value: Option<&proto::TypedValue>) -> bool {
    use proto::typed_value::Value;

    match value.and_then(|value| value.value.as_ref()) {
        Some(
            Value::StringVal(_)
            | Value::AsciiVal(_)
            | Value::IntVal(_)
            | Value::UintVal(_)
            | Value::BoolVal(_)
            | Value::FloatVal(_)
            | Value::DoubleVal(_),
        ) => true,
        Some(Value::JsonVal(bytes) | Value::JsonIetfVal(bytes)) => {
            serde_json::from_slice::<String>(bytes).is_ok()
                || serde_json::from_slice::<serde_json::Number>(bytes).is_ok()
                || serde_json::from_slice::<bool>(bytes).is_ok()
        }
        _ => false,
    }
}
