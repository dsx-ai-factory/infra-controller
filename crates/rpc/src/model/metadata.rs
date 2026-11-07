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

use carbide_utils::none_if_empty::NoneIfEmpty;
use model::metadata::{LabelFilter, Metadata};

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<Metadata> for rpc::Metadata {
    fn from(metadata: Metadata) -> Self {
        rpc::Metadata {
            name: metadata.name,
            description: metadata.description,
            labels: metadata
                .labels
                .iter()
                .map(|(key, value)| rpc::forge::Label {
                    key: key.clone(),
                    value: value.clone().none_if_empty(),
                })
                .collect(),
        }
    }
}

impl TryFrom<rpc::Metadata> for Metadata {
    type Error = RpcDataConversionError;

    fn try_from(metadata: rpc::Metadata) -> Result<Self, Self::Error> {
        let mut labels = std::collections::HashMap::new();

        for label in metadata.labels {
            let key = label.key.clone();
            let value = label.value.clone().unwrap_or_default();

            if labels.contains_key(&key) {
                return Err(RpcDataConversionError::InvalidLabel(format!(
                    "Duplicate key found: {key}"
                )));
            }

            labels.insert(key, value);
        }

        Ok(Metadata {
            name: metadata.name,
            description: metadata.description,
            labels,
        })
    }
}

impl From<rpc::forge::Label> for LabelFilter {
    fn from(label: rpc::forge::Label) -> Self {
        LabelFilter {
            key: label.key,
            value: label.value,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    // `LabelFilter::from` is a total conversion: project to (key, value) — the two
    // fields the originals asserted.
    #[test]
    fn label_filter_from_rpc() {
        value_scenarios!(
            run = |label| {
                let filter = LabelFilter::from(label);
                (filter.key, filter.value)
            };
            "with value" {
                rpc::forge::Label {
                    key: "env".to_string(),
                    value: Some("prod".to_string()),
                } => ("env".to_string(), Some("prod".to_string())),
            }

            "without value" {
                rpc::forge::Label {
                    key: "env".to_string(),
                    value: None,
                } => ("env".to_string(), None),
            }

            "empty key" {
                rpc::forge::Label {
                    key: String::new(),
                    value: Some("prod".to_string()),
                } => (String::new(), Some("prod".to_string())),
            }
        );
    }
}
