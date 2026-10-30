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

//! The REST inventory reader: every interval, the provider's site totals of
//! machines, racks and trays, and one machine page that checks the REST
//! site is this NICo site. Rack and tray totals need NICo Flow: a 412 marks
//! them unavailable, which is never a total of 0. The instance total is
//! always unavailable: the REST API has no provider-scoped instance count.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use carbide_site_health_probe_api::{Family, RestResource, SiteMatch};
use reqwest::StatusCode;

use crate::config::ReaderConfig;
use crate::framework::{ObservationRecorder, Probe};
use crate::readers::{Attempt, Failure, incomplete};
use crate::rest::auth::Auth;
use crate::rest::client::{RestClient, pagination_total};
use crate::snapshot::{CensusView, Reader, SnapshotSink, SweepMeta, unix_millis};

/// The REST page size cap.
const PAGE_SIZE: u64 = 100;

/// Every REST total, in label order.
pub(crate) const REST_RESOURCES: [RestResource; 4] = [
    RestResource::Machine,
    RestResource::Instance,
    RestResource::Rack,
    RestResource::Tray,
];

fn path(resource: RestResource) -> &'static str {
    match resource {
        RestResource::Machine => "machine",
        RestResource::Instance => "instance",
        RestResource::Rack => "rack",
        RestResource::Tray => "tray",
    }
}

/// The provider's site totals. `None` is unavailable: the site has no NICo
/// Flow for that resource (412), or the REST API has no provider-scoped
/// count of it (instances).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestTotalsData {
    pub totals: BTreeMap<RestResource, Option<u64>>,
}

/// Whether the REST site's machines are this NICo site's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SiteMatchData {
    pub result: SiteMatch,
}

/// The fields of a REST machine the site check reads.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestMachine {
    #[serde(default)]
    controller_machine_id: String,
}

/// Where the inventory reader reads, and as whom.
pub(crate) struct Inventory {
    pub provider_auth: Arc<Auth>,
    pub provider_org: String,
    pub site_id: String,
}

/// Reads site totals and the site check.
pub(crate) struct RestInventoryReader {
    cfg: ReaderConfig,
    client: Arc<RestClient>,
    target: Inventory,
    census: Arc<dyn CensusView>,
    sink: Arc<dyn SnapshotSink>,
    process: u64,
    seq: AtomicU64,
    attempt: Attempt,
}

impl RestInventoryReader {
    pub(crate) fn new(
        cfg: ReaderConfig,
        client: Arc<RestClient>,
        target: Inventory,
        census: Arc<dyn CensusView>,
        sink: Arc<dyn SnapshotSink>,
        process_start: SystemTime,
    ) -> Self {
        Self {
            cfg,
            client,
            target,
            census,
            sink,
            process: unix_millis(process_start),
            seq: AtomicU64::new(0),
            attempt: Attempt::new(
                Reader::RestInventory,
                &[Family::RestTotals, Family::RestSiteMatch],
            ),
        }
    }

    async fn totals(&self, rec: &ObservationRecorder) -> Result<RestTotalsData, Failure> {
        let mut totals = BTreeMap::new();
        for resource in REST_RESOURCES {
            if resource == RestResource::Instance {
                // The instance list needs a tenant admin and counts only that
                // tenant's instances, so it is never sent.
                totals.insert(resource, None);
                continue;
            }
            let reply = self
                .client
                .get(
                    rec,
                    "count_site_resources",
                    &self.target.provider_auth,
                    &self.target.provider_org,
                    &[path(resource)],
                    &[
                        ("siteId", self.target.site_id.clone()),
                        ("pageSize", "1".to_string()),
                    ],
                )
                .await
                .map_err(|e| Failure::call(path(resource), &e))?;
            let total = if reply.status == StatusCode::PRECONDITION_FAILED {
                // The site has no NICo Flow: no total exists, not a 0 one.
                None
            } else {
                Some(pagination_total(&reply).map_err(|e| Failure::call(path(resource), &e))?)
            };
            totals.insert(resource, total);
        }
        Ok(RestTotalsData { totals })
    }

    /// One machine page against the NICo census: the REST site matches only
    /// when every machine on a nonempty page names a NICo machine of this
    /// census, and mismatches when none does. A page with a machine that
    /// names no NICo machine, or a mix (a machine ingested after the census
    /// could explain it), is unknown: missing evidence never makes a match.
    async fn site_match(&self, rec: &ObservationRecorder) -> Result<SiteMatchData, Failure> {
        let reply = self
            .client
            .get(
                rec,
                "list_site_machines",
                &self.target.provider_auth,
                &self.target.provider_org,
                &["machine"],
                &[
                    ("siteId", self.target.site_id.clone()),
                    ("pageSize", PAGE_SIZE.to_string()),
                ],
            )
            .await
            .map_err(|e| Failure::call("machine page", &e))?;
        let machines: Vec<RestMachine> = reply
            .json()
            .map_err(|e| Failure::call("machine page", &e))?;
        let ids: Vec<String> = machines
            .into_iter()
            .map(|m| m.controller_machine_id)
            .collect();
        let result = match self.census.machine_ids() {
            _ if ids.is_empty() || ids.iter().any(String::is_empty) => SiteMatch::Unknown,
            None => SiteMatch::Unknown,
            Some(census) => {
                let known = ids.iter().filter(|id| census.contains(*id)).count();
                if known == ids.len() {
                    SiteMatch::Match
                } else if known == 0 {
                    SiteMatch::Mismatch
                } else {
                    SiteMatch::Unknown
                }
            }
        };
        Ok(SiteMatchData { result })
    }
}

#[async_trait::async_trait]
impl Probe for RestInventoryReader {
    fn name(&self) -> &'static str {
        Reader::RestInventory.as_str()
    }

    fn api(&self) -> &'static str {
        "nico-rest-api"
    }

    fn interval(&self) -> Duration {
        self.cfg.interval
    }

    fn timeout(&self) -> Duration {
        self.cfg.timeout
    }

    fn timed_out(&self) {
        self.attempt.timed_out(self.sink.as_ref());
    }

    async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
        let start = crate::clock::now();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.attempt.begin();
        let totals = self.totals(recorder).await;
        let site_match = self.site_match(recorder).await;
        let meta = SweepMeta {
            process: self.process,
            seq,
            reader: Reader::RestInventory,
            start,
            end: crate::clock::now(),
        };
        let sink = self.sink.as_ref();
        let mut failed = Vec::new();
        if !self
            .attempt
            .deliver(sink, &meta, Family::RestTotals, totals)
        {
            failed.push(Family::RestTotals);
        }
        if !self
            .attempt
            .deliver(sink, &meta, Family::RestSiteMatch, site_match)
        {
            failed.push(Family::RestSiteMatch);
        }
        incomplete(&failed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::rest::fake::FakeRest;
    use crate::snapshot::testing::FakeSink;

    /// A census that tests set directly.
    #[derive(Default)]
    struct FixedCensus(Mutex<Option<BTreeSet<String>>>);

    impl CensusView for FixedCensus {
        fn machine_ids(&self) -> Option<BTreeSet<String>> {
            self.0.lock().unwrap().clone()
        }
    }

    fn reader(
        rest: &FakeRest,
        sink: &Arc<FakeSink>,
        census: Option<&[&str]>,
    ) -> RestInventoryReader {
        let fixed = FixedCensus::default();
        *fixed.0.lock().unwrap() = census.map(|ids| ids.iter().map(|id| id.to_string()).collect());
        RestInventoryReader::new(
            ReaderConfig {
                enabled: true,
                interval: Duration::from_secs(120),
                timeout: Duration::from_secs(600),
            },
            rest.client(),
            Inventory {
                provider_auth: rest.auth(),
                provider_org: "provider-org".to_string(),
                site_id: "site-1".to_string(),
            },
            Arc::new(fixed),
            Arc::clone(sink) as Arc<dyn SnapshotSink>,
            SystemTime::now(),
        )
    }

    fn totals(sink: &FakeSink) -> BTreeMap<RestResource, Option<u64>> {
        sink.latest::<RestTotalsData>(Family::RestTotals)
            .expect("totals")
            .1
            .totals
    }

    fn site_match(sink: &FakeSink) -> SiteMatch {
        sink.latest::<SiteMatchData>(Family::RestSiteMatch)
            .expect("site match")
            .1
            .result
    }

    #[tokio::test]
    async fn rest_total_reads_x_pagination() {
        let rest = FakeRest::start().await;
        rest.set_total("machine", 4500);
        rest.always_status("instance", StatusCode::FORBIDDEN);
        rest.set_total("rack", 250);
        rest.set_total("tray", 9000);
        let sink = Arc::new(FakeSink::default());
        reader(&rest, &sink, None)
            .run(&ObservationRecorder::default())
            .await
            .expect("sweep");
        assert_eq!(
            totals(&sink),
            BTreeMap::from([
                (RestResource::Machine, Some(4500)),
                (RestResource::Instance, None),
                (RestResource::Rack, Some(250)),
                (RestResource::Tray, Some(9000)),
            ])
        );
        let page_of_one = rest
            .requests()
            .into_iter()
            .filter(|(_, path, query)| {
                path.starts_with("/v2/org/provider-org/")
                    && query.split('&').any(|param| param == "pageSize=1")
            })
            .count();
        assert_eq!(page_of_one, 3, "one pageSize=1 read per supported total");
        assert!(
            rest.requests()
                .iter()
                .all(|(_, path, _)| !path.ends_with("/instance")),
            "the tenant-scoped instance list is never called"
        );
        assert_eq!(
            site_match(&sink),
            SiteMatch::Unknown,
            "the site check publishes"
        );
    }

    #[tokio::test]
    async fn forbidden_or_missing_supported_total_fails_the_sweep() {
        for status in [StatusCode::FORBIDDEN, StatusCode::NOT_FOUND] {
            let rest = FakeRest::start().await;
            rest.set_total("machine", 5);
            let sink = Arc::new(FakeSink::default());
            let reader = reader(&rest, &sink, None);
            reader
                .run(&ObservationRecorder::default())
                .await
                .expect("first sweep");
            rest.always_status("rack", status);
            assert!(
                reader.run(&ObservationRecorder::default()).await.is_err(),
                "{status}"
            );
            assert_eq!(
                sink.count(Family::RestTotals),
                1,
                "the last totals are kept"
            );
            assert_eq!(sink.failure_kinds(Family::RestTotals).len(), 1, "{status}");
        }
    }

    #[tokio::test]
    async fn rack_total_412_is_unavailable_not_zero() {
        let rest = FakeRest::start().await;
        rest.set_total("machine", 10);
        rest.set_total("instance", 3);
        rest.always_status("rack", StatusCode::PRECONDITION_FAILED);
        rest.always_status("tray", StatusCode::PRECONDITION_FAILED);
        let sink = Arc::new(FakeSink::default());
        reader(&rest, &sink, None)
            .run(&ObservationRecorder::default())
            .await
            .expect("a 412 does not fail the family");
        let totals = totals(&sink);
        assert_eq!(totals[&RestResource::Rack], None);
        assert_eq!(totals[&RestResource::Tray], None);
        assert_eq!(totals[&RestResource::Machine], Some(10));
    }

    #[tokio::test]
    async fn rest_total_without_header_keeps_last() {
        let rest = FakeRest::start().await;
        rest.set_total("machine", 5);
        let sink = Arc::new(FakeSink::default());
        let reader = reader(&rest, &sink, None);
        reader
            .run(&ObservationRecorder::default())
            .await
            .expect("first sweep");
        rest.omit_pagination_header(true);
        assert!(reader.run(&ObservationRecorder::default()).await.is_err());
        assert_eq!(
            sink.count(Family::RestTotals),
            1,
            "the last totals are kept"
        );
        assert_eq!(
            sink.failure_kinds(Family::RestTotals),
            vec![crate::snapshot::FailureKind::Decode]
        );
    }

    #[tokio::test]
    async fn site_match_unknown_on_empty_page() {
        let rest = FakeRest::start().await;
        let sink = Arc::new(FakeSink::default());
        reader(&rest, &sink, Some(&["fm100a"]))
            .run(&ObservationRecorder::default())
            .await
            .expect("sweep");
        assert_eq!(site_match(&sink), SiteMatch::Unknown);

        // Machines on the page, but no device sweep yet.
        rest.set_pages(
            "machine",
            vec![vec![json!({"controllerMachineId": "fm100a"})]],
        );
        let sink = Arc::new(FakeSink::default());
        reader(&rest, &sink, None)
            .run(&ObservationRecorder::default())
            .await
            .expect("sweep");
        assert_eq!(site_match(&sink), SiteMatch::Unknown);
    }

    #[tokio::test]
    async fn site_match_mismatch_when_id_not_in_census() {
        let rest = FakeRest::start().await;
        rest.set_pages(
            "machine",
            vec![vec![
                json!({"id": "m-1", "controllerMachineId": "fm100other"}),
            ]],
        );
        let sink = Arc::new(FakeSink::default());
        reader(&rest, &sink, Some(&["fm100a", "fm100b"]))
            .run(&ObservationRecorder::default())
            .await
            .expect("sweep");
        assert_eq!(site_match(&sink), SiteMatch::Mismatch);

        let census = Some(&["fm100a", "fm100b"][..]);
        for (page, want, why) in [
            (
                vec![
                    json!({"controllerMachineId": "fm100new"}),
                    json!({"controllerMachineId": "fm100b"}),
                ],
                SiteMatch::Unknown,
                "one known machine does not vouch for the other",
            ),
            (
                vec![
                    json!({"controllerMachineId": "fm100a"}),
                    json!({"id": "m-2"}),
                ],
                SiteMatch::Unknown,
                "a machine without a NICo ID is missing evidence",
            ),
            (
                vec![
                    json!({"controllerMachineId": "fm100a"}),
                    json!({"controllerMachineId": "fm100b"}),
                ],
                SiteMatch::Match,
                "every machine is a NICo machine of this site",
            ),
        ] {
            rest.set_pages("machine", vec![page]);
            reader(&rest, &sink, census)
                .run(&ObservationRecorder::default())
                .await
                .expect("sweep");
            assert_eq!(site_match(&sink), want, "{why}");
        }
    }

    #[tokio::test]
    async fn rest_readers_send_get_only() {
        let rest = FakeRest::start().await;
        rest.set_total("instance", 1);
        let sink = Arc::new(FakeSink::default());
        reader(&rest, &sink, None)
            .run(&ObservationRecorder::default())
            .await
            .expect("inventory sweep");
        let requests = rest.requests();
        assert!(!requests.is_empty());
        for (method, path, _) in requests {
            assert_eq!(method, "GET", "{path}");
        }
    }

    #[tokio::test]
    async fn rest_backs_off_on_429_and_503() {
        let rest = FakeRest::start().await;
        rest.set_total("machine", 1);
        rest.queue_status("machine", StatusCode::TOO_MANY_REQUESTS, None);
        rest.queue_status("machine", StatusCode::SERVICE_UNAVAILABLE, Some(2));
        let sink = Arc::new(FakeSink::default());
        reader(&rest, &sink, None)
            .run(&ObservationRecorder::default())
            .await
            .expect("the retries pass");
        assert_eq!(totals(&sink)[&RestResource::Machine], Some(1));
        let starts = rest.starts();
        // 429: back off 1 s. 503 with Retry-After 2: back off 2 s.
        assert!(
            starts[1] - starts[0] >= Duration::from_secs(1),
            "{starts:?}"
        );
        assert!(
            starts[2] - starts[1] >= Duration::from_secs(2),
            "{starts:?}"
        );
    }

    #[tokio::test]
    async fn rest_caps_hold_against_slow_server() {
        let rest = FakeRest::start().await;
        rest.set_total("instance", 1);
        rest.set_delay(Duration::from_millis(150));
        let sink = Arc::new(FakeSink::default());
        // Two readers on one client share its gate.
        let (first, second) = (reader(&rest, &sink, None), reader(&rest, &sink, None));
        let rec = ObservationRecorder::default();
        let (a, b) = tokio::join!(first.run(&rec), second.run(&rec));
        a.expect("first sweep");
        b.expect("second sweep");
        assert_eq!(
            rest.max_in_flight(),
            1,
            "one REST call in flight across readers"
        );
        let mut starts = rest.starts();
        starts.sort();
        for pair in starts.windows(2) {
            assert!(
                pair[1] - pair[0] >= Duration::from_millis(100),
                "calls started {:?} apart",
                pair[1] - pair[0]
            );
        }
    }

    #[test]
    fn no_rms_dependency() {
        let manifest = include_str!("../../Cargo.toml");
        for line in manifest.lines() {
            let name = line.split('=').next().unwrap_or_default().trim();
            assert!(
                !name.contains("rms"),
                "the probe must not depend on RMS: {line}"
            );
        }
    }
}
