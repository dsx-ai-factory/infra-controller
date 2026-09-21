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

use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ufm_mock::InventorySourceConfig;
use url::Url;

use crate::config::SourceClientConfig;

/// Route every machine-a-tron serves its inventory snapshot on.
const STATUS_PATH: &str = "/machines/status";

/// Body of `GET /v1/sources` served by the Go controller.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SourceList {
    /// Version counter of the source set; see [`SourceListCheck`] for what it does and does not
    /// tell the gateway.
    pub generation: u64,
    /// True once the controller completed its first discovery pass.
    pub ready: bool,
    /// Discovered instances, sorted by name by the controller.
    #[serde(default)]
    pub sources: Vec<Source>,
}

/// One machine-a-tron instance reachable from the gateway.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Source {
    /// Stable machine-a-tron identity, normally its Service name.
    pub name: String,
    /// Scheme, host, and port of the instance's control server.
    pub base_url: Url,
    /// Pod currently backing the Service, or empty when unknown.
    ///
    /// Logged at bootstrap so an operator can map a source to the pod behind it. It is not part
    /// of the source set identity: a pod restart behind an unchanged Service URL is not a change.
    #[serde(default)]
    pub pod: String,
}

/// Why a parsed [`SourceList`] cannot seed reconciliation.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SourceListError {
    #[error("controller has not completed its initial discovery")]
    NotReady,
    #[error("controller reported no machine-a-tron sources")]
    Empty,
}

impl SourceList {
    /// Deserializes a `GET /v1/sources` body without validating it.
    pub fn parse(body: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(body)
    }

    /// Checks that the list can seed UFM reconciliation: the controller is ready and reports at
    /// least one source.
    pub fn validate(&self) -> Result<(), SourceListError> {
        if !self.ready {
            return Err(SourceListError::NotReady);
        }
        if self.sources.is_empty() {
            return Err(SourceListError::Empty);
        }
        Ok(())
    }

    /// Derives the UFM static inventory sources: one `/machines/status` URL per source.
    pub fn inventory_sources(&self, client: &SourceClientConfig) -> Vec<InventorySourceConfig> {
        self.sources
            .iter()
            .map(|source| InventorySourceConfig {
                url: source.status_url(),
                ca_cert_path: client.ca_cert_path.clone(),
                insecure_skip_verify: client.insecure_skip_verify,
            })
            .collect()
    }

    /// Source names in list order, for logs and assertions.
    pub fn names(&self) -> Vec<&str> {
        self.sources
            .iter()
            .map(|source| source.name.as_str())
            .collect()
    }

    /// Backing pod names in list order, parallel to [`Self::names`]; empty where the controller
    /// has not resolved the pod.
    pub(crate) fn pods(&self) -> Vec<&str> {
        self.sources
            .iter()
            .map(|source| source.pod.as_str())
            .collect()
    }

    /// The `(name, base_url)` set this list describes; see [`SourceSet`].
    fn source_set(&self) -> SourceSet {
        SourceSet(
            self.sources
                .iter()
                .map(|source| (source.name.clone(), source.base_url.clone()))
                .collect(),
        )
    }
}

impl Source {
    /// Joins [`STATUS_PATH`] onto the base URL, keeping any path prefix the base URL carries.
    fn status_url(&self) -> Url {
        let mut url = self.base_url.clone();
        let base_path = url.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}{STATUS_PATH}"));
        url.set_query(None);
        url.set_fragment(None);
        url
    }
}

/// Identity of a source set: the sorted `(name, base_url)` pairs of a [`SourceList`].
///
/// This is the same key the controller uses to decide whether to bump its generation. Pod names
/// and list order are excluded, so a pod restart behind an unchanged Service URL and a
/// re-sorted list both compare equal. Two controller processes that discover the same fleet
/// produce equal sets even though their generation counters differ.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceSet(BTreeSet<(String, Url)>);

impl SourceSet {
    /// Names of the pairs in `self` that `other` lacks, in name order.
    ///
    /// A source whose URL changed keeps its name, so it shows up both as missing from the old set
    /// and as missing from the new one.
    fn names_missing_from(&self, other: &Self) -> Vec<String> {
        self.0
            .difference(&other.0)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Difference between the set the gateway started with and the controller's current set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSetChange {
    /// Generation the gateway bootstrapped from.
    pub startup: u64,
    /// Generation the controller publishes now. It may equal `startup` after a controller
    /// restart, which is why the set and not the counter decides.
    pub current: u64,
    /// Names present now but not at startup.
    pub added: Vec<String>,
    /// Names present at startup but not now. A URL change lists the name here and in `added`.
    pub removed: Vec<String>,
}

/// Result of comparing the controller's current list against the one the gateway started with.
///
/// The source set decides whether the gateway must restart. The generation is reported as a hint
/// for logs and never decides on its own: the controller's counter restarts from zero with the
/// controller container, so it moves while the set is unchanged and can repeat a value for a
/// different set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceListCheck {
    /// Same set, same generation.
    Unchanged,
    /// The controller has not completed a discovery pass since it (re)started. Its list is empty
    /// and says nothing about the fleet, so the gateway keeps serving the set it has.
    NotReady { current: u64 },
    /// Same set under a different generation: the controller restarted or republished.
    GenerationMoved { current: u64 },
    /// A source was added, removed, or changed URL.
    SetChanged(SourceSetChange),
}

impl SourceListCheck {
    /// Compares `current` against `startup`; see the variants for the rules.
    pub fn compare(startup: &SourceList, current: &SourceList) -> Self {
        if !current.ready {
            return Self::NotReady {
                current: current.generation,
            };
        }
        let before = startup.source_set();
        let after = current.source_set();
        if before != after {
            return Self::SetChanged(SourceSetChange {
                startup: startup.generation,
                current: current.generation,
                added: after.names_missing_from(&before),
                removed: before.names_missing_from(&after),
            });
        }
        if startup.generation != current.generation {
            return Self::GenerationMoved {
                current: current.generation,
            };
        }
        Self::Unchanged
    }
}

/// HTTP client for the controller's pod-local source list.
#[derive(Clone, Debug)]
pub struct SourceListClient {
    /// Endpoint the client fetches, kept for log context.
    pub(crate) url: Url,
    client: reqwest::Client,
}

impl SourceListClient {
    /// Builds a plain HTTP client with a per-request `timeout`.
    pub fn new(url: Url, timeout: Duration) -> eyre::Result<Self> {
        let client = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self { url, client })
    }

    /// Fetches and parses the list; non-2xx responses and malformed bodies are errors.
    pub async fn fetch(&self) -> eyre::Result<SourceList> {
        let response = self
            .client
            .get(self.url.as_str())
            .send()
            .await?
            .error_for_status()?;
        let body = response.bytes().await?;
        SourceList::parse(&body)
            .map_err(|error| eyre::eyre!("invalid source list from {}: {error}", self.url))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};

    use super::*;

    /// The controller's checked-in contract fixture, produced by the Go handler test
    /// `TestHandler_SourcesGolden`. Field parity with it is checked in
    /// `tests/integration/source_list_contract.rs`; these tests use it as realistic input.
    const FIXTURE: &str = include_str!(
        "../../../dev/k8s/machine-a-tron-controller/pkg/sourcelist/testdata/sources_v1.json"
    );
    const MAT_0: &str = "nico-machine-a-tron-mat-0-bmc-mock";
    const MAT_1: &str = "nico-machine-a-tron-mat-1-bmc-mock";
    const SINGLE: &str = "nico-machine-a-tron-single-bmc-mock";

    fn fixture() -> SourceList {
        SourceList::parse(FIXTURE.as_bytes()).unwrap()
    }

    #[test]
    fn parses_the_controller_contract_fixture() {
        let list = fixture();

        assert_eq!(list.generation, 3);
        assert!(list.ready);
        assert_eq!(list.names(), vec![MAT_0, MAT_1, SINGLE]);
        assert_eq!(list.pods(), vec!["mat-0", "mat-1", ""]);
        list.validate().unwrap();
    }

    #[test]
    fn missing_pod_field_defaults_to_empty() {
        let list = SourceList::parse(
            br#"{"generation": 1, "ready": true, "sources": [{"name": "a", "base_url": "https://a:1266"}]}"#,
        )
        .unwrap();

        assert_eq!(list.pods(), vec![""]);
    }

    #[test]
    fn derives_one_status_url_per_source() {
        let client = SourceClientConfig {
            insecure_skip_verify: true,
            ..SourceClientConfig::default()
        };

        let inventory = fixture().inventory_sources(&client);

        assert_eq!(inventory.len(), 3);
        assert_eq!(
            inventory[0].url.as_str(),
            "https://nico-machine-a-tron-mat-0-bmc-mock.nico-system.svc.cluster.local:8443/machines/status"
        );
        assert_eq!(
            inventory[2].url.as_str(),
            "https://nico-machine-a-tron-single-bmc-mock.nico-system.svc.cluster.local:1266/machines/status"
        );
        assert!(inventory.iter().all(|source| source.insecure_skip_verify));
    }

    #[test]
    fn status_url_preserves_base_path_prefix() {
        let source = Source {
            name: "a".into(),
            base_url: Url::parse("https://a.example:1266/prefix/?x=1").unwrap(),
            pod: String::new(),
        };

        assert_eq!(
            source.status_url().as_str(),
            "https://a.example:1266/prefix/machines/status"
        );
    }

    fn mutated(mutate: fn(&mut SourceList)) -> SourceList {
        let mut list = fixture();
        mutate(&mut list);
        list
    }

    #[test]
    fn validate_gates_startup() {
        check_cases(
            [
                Case {
                    scenario: "ready list from the contract fixture",
                    input: fixture(),
                    expect: Yields(()),
                },
                Case {
                    scenario: "controller not ready",
                    input: mutated(|list| list.ready = false),
                    expect: FailsWith(SourceListError::NotReady),
                },
                Case {
                    scenario: "no sources",
                    input: mutated(|list| list.sources.clear()),
                    expect: FailsWith(SourceListError::Empty),
                },
            ],
            |list| list.validate(),
        );
    }

    #[test]
    fn source_set_ignores_pod_names_and_order() {
        let list = fixture();
        let reordered = mutated(|list| list.sources.reverse());
        let repodded = mutated(|list| {
            list.sources[0].pod = "nico-machine-a-tron-mat-0-5d4c3b2a1-zyxwv".into();
            list.sources[1].pod.clear();
            list.sources[2].pod = "nico-machine-a-tron-single-5d4c3b2a1-vwxyz".into();
        });

        assert_eq!(list.source_set().0.len(), 3);
        assert_eq!(list.source_set(), reordered.source_set());
        assert_eq!(list.source_set(), repodded.source_set());
    }

    #[test]
    fn source_set_decides_and_generation_is_only_a_hint() {
        const MAT_2: &str = "nico-machine-a-tron-mat-2-bmc-mock";
        let startup = fixture();
        let generation = startup.generation;
        assert_eq!(
            generation, 3,
            "fixture generation drives the expectations below"
        );
        let changed = |current: u64, added: &[&str], removed: &[&str]| {
            SourceListCheck::SetChanged(SourceSetChange {
                startup: generation,
                current,
                added: added.iter().map(ToString::to_string).collect(),
                removed: removed.iter().map(ToString::to_string).collect(),
            })
        };
        fn mat_2() -> Source {
            Source {
                name: MAT_2.into(),
                base_url: Url::parse("https://mat-2.nico-system.svc.cluster.local:8443").unwrap(),
                pod: String::new(),
            }
        }

        check_values(
            [
                Check {
                    scenario: "identical list",
                    input: fixture(),
                    expect: SourceListCheck::Unchanged,
                },
                Check {
                    scenario: "pod names changed after machine-a-tron pod restarts",
                    input: mutated(|list| list.sources[1].pod = "replacement-pod".into()),
                    expect: SourceListCheck::Unchanged,
                },
                Check {
                    scenario: "controller restarted and republished the same set as generation 1",
                    input: mutated(|list| list.generation = 1),
                    expect: SourceListCheck::GenerationMoved { current: 1 },
                },
                Check {
                    scenario: "controller rediscovered the same set under a higher generation",
                    input: mutated(|list| list.generation = 12),
                    expect: SourceListCheck::GenerationMoved { current: 12 },
                },
                Check {
                    scenario: "controller restarting: generation 0, not ready, no sources",
                    input: SourceList {
                        generation: 0,
                        ready: false,
                        sources: Vec::new(),
                    },
                    expect: SourceListCheck::NotReady { current: 0 },
                },
                Check {
                    scenario: "source added under the repeated startup generation",
                    input: mutated(|list| list.sources.push(mat_2())),
                    expect: changed(3, &[MAT_2], &[]),
                },
                Check {
                    scenario: "source added with a bumped generation",
                    input: mutated(|list| {
                        list.generation = 4;
                        list.sources.push(mat_2());
                    }),
                    expect: changed(4, &[MAT_2], &[]),
                },
                Check {
                    scenario: "sources removed after a controller restart reset the counter",
                    input: mutated(|list| {
                        list.generation = 1;
                        list.sources.truncate(1);
                    }),
                    expect: changed(1, &[], &[MAT_1, SINGLE]),
                },
                Check {
                    scenario: "source changed URL",
                    input: mutated(|list| {
                        list.sources[0].base_url =
                            Url::parse("https://mat-0.other.svc.cluster.local:8443").unwrap();
                    }),
                    expect: changed(3, &[MAT_0], &[MAT_0]),
                },
                Check {
                    scenario: "every source gone but the controller is ready",
                    input: mutated(|list| list.sources.clear()),
                    expect: changed(3, &[], &[MAT_0, MAT_1, SINGLE]),
                },
            ],
            |current| SourceListCheck::compare(&startup, &current),
        );
    }
}
