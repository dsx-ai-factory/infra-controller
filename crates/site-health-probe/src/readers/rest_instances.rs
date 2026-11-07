/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! The optional single-tenant fixture, published independently per complete read.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use carbide_site_health_probe_api::{
    Family, InstanceFields, RestMachineFields, Source, Stage, TenantAllocation, Unit, UnitType,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::config::TenantAccount;
use crate::framework::ObservationRecorder;
use crate::readers::rest_inventory::Inventory;
use crate::readers::{Attempt, Failure, Incomplete};
use crate::rest::auth::Auth;
use crate::rest::client::{RestClient, pagination_total};
use crate::snapshot::{SnapshotSink, SweepMeta, UnitsBuilder, UnitsData};

pub(crate) const FAMILIES: [Family; 3] = [
    Family::RestInstances,
    Family::RestMachines,
    Family::ConfigRest,
];
// A complete cached read has bounded rows, bytes and pages. Exceeding a cap is
// unavailable, never a truncated census. There is no snapshot across REST pages.
const MAX_ROWS: usize = 200_000;
const MAX_BYTES: usize = 64 << 20;

pub(crate) struct Fixture {
    pub config: TenantAccount,
    pub auth: Arc<Auth>,
    /// Each family's last built units, which the next sweep shares where
    /// they did not change.
    pub previous: Mutex<BTreeMap<Family, Vec<Arc<Unit>>>>,
}

pub(crate) struct Publication<'a> {
    pub meta: &'a SweepMeta,
    pub attempt: &'a Attempt,
    pub sink: &'a dyn SnapshotSink,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Instance {
    id: String,
    /// Null until the site controller reconciles the instance.
    controller_instance_id: Option<String>,
    name: String,
    tenant_id: String,
    site_id: String,
    machine_id: Option<String>,
    status: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Machine {
    id: String,
    controller_machine_id: String,
    site_id: String,
    instance_type_id: Option<String>,
    instance_id: Option<String>,
    tenant_id: Option<String>,
    #[serde(deserialize_with = "nullable_labels")]
    labels: BTreeMap<String, String>,
    is_usable_by_tenant: bool,
    status: String,
}
fn nullable_labels<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    Ok(Option::<BTreeMap<String, String>>::deserialize(d)?.unwrap_or_default())
}
fn invalid() -> Failure {
    Failure::unresolved(
        "REST fixture identity, paging or allocation evidence is incomplete".to_string(),
    )
}

// Shared capped pagination checks identity, totals and exact page lengths.
// REST owns collation. Matching totals cannot prove a transactional snapshot,
// so the runner must require settled repeated agreement before clearing work.
async fn pages<T: DeserializeOwned>(
    client: &RestClient,
    rec: &ObservationRecorder,
    auth: &Auth,
    org: &str,
    site: &str,
    path: &str,
    id: fn(&T) -> &str,
) -> Result<Vec<T>, Failure> {
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    let mut total = None;
    let mut bytes = 0;
    for page in 1..=MAX_ROWS / 100 + 1 {
        let reply = client
            .get(
                rec,
                "fixture_page",
                auth,
                org,
                &[path],
                &[
                    ("siteId", site.to_string()),
                    ("pageSize", "100".into()),
                    ("pageNumber", page.to_string()),
                    (
                        "orderBy",
                        if path == "instance" {
                            "NAME_ASC".into()
                        } else {
                            "ID_ASC".into()
                        },
                    ),
                ],
            )
            .await
            .map_err(|e| Failure::call("fixture_page", &e))?;
        bytes += reply.body.len();
        if bytes > MAX_BYTES {
            return Err(invalid());
        }
        let n = pagination_total(&reply).map_err(|e| Failure::call("fixture_page", &e))?;
        if n > MAX_ROWS as u64 || total.is_some_and(|old| old != n) {
            return Err(invalid());
        }
        total = Some(n);
        let batch: Vec<T> = reply
            .json()
            .map_err(|e| Failure::call("fixture_page", &e))?;
        let remaining = (n as usize).checked_sub(rows.len()).ok_or_else(invalid)?;
        if batch.len() != remaining.min(100) {
            return Err(invalid());
        }
        for row in &batch {
            let key = id(row);
            if key.is_empty() || !ids.insert(key.to_string()) {
                return Err(invalid());
            }
        }
        rows.extend(batch);
        if rows.len() == n as usize {
            return Ok(rows);
        }
    }
    Err(invalid())
}

fn unit(id: String, kind: UnitType, status: String) -> Unit {
    let ready = status == "Ready";
    let failed = status == "Error";
    Unit {
        controller_instance_id: None,
        id,
        unit_type: kind,
        source: Source::Rest,
        stage: if ready {
            Stage::Ready
        } else if failed {
            Stage::Error
        } else {
            Stage::Registered
        },
        done: ready,
        state: status,
        reason: None,
        bmc_ip: None,
        host: None,
        dpu: None,
        instance: None,
        expected: None,
        rest_machine: None,
    }
}
impl Fixture {
    /// Builds `family` against its last built units.
    fn build(&self, family: Family, units: Vec<Unit>) -> UnitsData {
        let mut previous = self.previous.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = UnitsBuilder::new(previous.get(&family).map_or(&[], Vec::as_slice));
        out.extend(units);
        let data = out.finish(0);
        previous.insert(family, data.units.clone());
        data
    }
    async fn instances(
        &self,
        client: &RestClient,
        rec: &ObservationRecorder,
        site: &str,
    ) -> Result<UnitsData, Failure> {
        let rows = pages::<Instance>(
            client,
            rec,
            &self.auth,
            &self.config.org,
            site,
            "instance",
            |r| &r.id,
        )
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            if r.site_id != site || r.tenant_id != self.config.tenant_id || r.name.is_empty() {
                return Err(invalid());
            }
            let mut u = unit(r.id, UnitType::Instance, r.status.clone());
            // REST's id and controllerInstanceId may differ. Keep both for reconciliation.
            u.instance = Some(InstanceFields {
                name: Some(r.name),
                tenant: Some(r.tenant_id),
                host_id: r.machine_id,
                status: r.status,
            });
            // The additive field is separate from the REST identity used for DELETE.
            // The REST API sends null or "" until the controller knows the instance.
            u.controller_instance_id = r.controller_instance_id.filter(|id| !id.is_empty());
            out.push(u);
        }
        Ok(self.build(Family::RestInstances, out))
    }
    async fn machines(
        &self,
        client: &RestClient,
        rec: &ObservationRecorder,
        site: &str,
        provider_auth: &Auth,
        provider_org: &str,
    ) -> Result<UnitsData, Failure> {
        let rows = pages::<Machine>(
            client,
            rec,
            provider_auth,
            provider_org,
            site,
            "machine",
            |r| &r.id,
        )
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        let mut controllers = BTreeSet::new();
        for r in rows {
            if r.site_id != site
                || r.controller_machine_id.is_empty()
                || !controllers.insert(r.controller_machine_id.clone())
            {
                return Err(invalid());
            }
            let mut u = unit(r.id, UnitType::Host, r.status);
            u.rest_machine = Some(RestMachineFields {
                controller_id: r.controller_machine_id,
                instance_type: r.instance_type_id,
                instance_id: r.instance_id,
                tenant_id: r.tenant_id,
                labels: r.labels,
                is_usable_by_tenant: r.is_usable_by_tenant,
            });
            out.push(u);
        }
        Ok(self.build(Family::RestMachines, out))
    }
    async fn allocation(
        &self,
        client: &RestClient,
        rec: &ObservationRecorder,
        site: &str,
        provider: &str,
    ) -> Result<TenantAllocation, Failure> {
        #[derive(Deserialize)]
        struct Tenant {
            id: String,
            org: String,
        }
        let reply = client
            .get(
                rec,
                "fixture_tenant",
                &self.auth,
                &self.config.org,
                &["tenant", "current"],
                &[],
            )
            .await
            .map_err(|e| Failure::call("fixture_tenant", &e))?;
        let t: Tenant = reply
            .json()
            .map_err(|e| Failure::call("fixture_tenant", &e))?;
        if t.id != self.config.tenant_id || t.org != self.config.org {
            return Err(invalid());
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stats {
            assigned: i64,
            total: i64,
            used: i64,
            unused: i64,
            unused_usable: i64,
        }
        #[derive(Deserialize)]
        struct Provider {
            org: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Type {
            id: String,
            site_id: String,
            infrastructure_provider: Provider,
            allocation_stats: Stats,
        }
        let reply = client
            .get(
                rec,
                "fixture_allocation",
                &self.auth,
                &self.config.org,
                &["instance", "type", &self.config.instance_type],
                &[
                    ("includeAllocationStats", "true".into()),
                    ("includeRelation", "InfrastructureProvider".into()),
                ],
            )
            .await
            .map_err(|e| Failure::call("fixture_allocation", &e))?;
        let r: Type = reply
            .json()
            .map_err(|e| Failure::call("fixture_allocation", &e))?;
        let a = r.allocation_stats;
        // Provider-authorized stats are site-wide, not tenant quota. Require the
        // configured distinct provider org in the returned ownership relation.
        if r.id != self.config.instance_type
            || r.site_id != site
            || r.infrastructure_provider.org != provider
            || provider == self.config.org
            || [a.assigned, a.total, a.used, a.unused, a.unused_usable]
                .iter()
                .any(|n| *n < 0)
            || a.used > a.total
            || a.used > a.assigned
            || a.total - a.used != a.unused
            || a.unused_usable > a.unused
        {
            return Err(invalid());
        }
        Ok(TenantAllocation {
            tenant: t.id,
            instance_type: r.id,
            assigned: a.assigned,
            total: a.total,
            used: a.used,
            unused: a.unused,
            unused_usable: a.unused_usable,
        })
    }
    pub(crate) async fn read(
        &self,
        client: &RestClient,
        rec: &ObservationRecorder,
        target: &Inventory,
        publication: Publication<'_>,
        failed: &mut Incomplete,
    ) {
        let Publication {
            meta: base,
            attempt,
            sink,
        } = publication;
        let site = &target.site_id;
        let provider = &target.provider_org;
        macro_rules! publish {
            ($family:expr,$read:expr) => {{
                let start = crate::clock::now();
                let result = $read.await;
                let kind = result.as_ref().err().map(|e| e.kind);
                let meta = SweepMeta {
                    start,
                    end: crate::clock::now(),
                    ..base.clone()
                };
                if !attempt.deliver(sink, &meta, $family, result) {
                    failed.record($family, kind)
                }
            }};
        }
        publish!(Family::RestInstances, self.instances(client, rec, site));
        publish!(
            Family::RestMachines,
            self.machines(client, rec, site, &target.provider_auth, provider)
        );
        publish!(
            Family::ConfigRest,
            self.allocation(client, rec, site, provider)
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::*;
    use crate::config::{ReaderConfig, RestCredentials};
    use crate::framework::{Outcome, Probe};
    use crate::readers::rest_inventory::RestInventoryReader;
    use crate::rest::fake::FakeRest;
    use crate::snapshot::testing::FakeSink;
    use crate::snapshot::{FailureKind, Reader};

    fn machine(i: usize) -> Value {
        json!({"id":format!("m-{i:03}"),"controllerMachineId":format!("host-{i}"),"siteId":"site-1","instanceTypeId":"type-1","instanceId":null,"tenantId":null,"labels":{"keep":"old","scale-test":"burst-1"},"isUsableByTenant":true,"status":"Ready"})
    }
    fn instance() -> Value {
        json!({"id":"rest-id","controllerInstanceId":"core-id","name":"owned-1","siteId":"site-1","tenantId":"tenant-id","machineId":"m-000","status":"Ready"})
    }
    fn fixture(rest: &FakeRest) -> Fixture {
        Fixture {
            config: TenantAccount {
                org: "tenant-org".into(),
                tenant_id: "tenant-id".into(),
                instance_type: "type-1".into(),
                auth: RestCredentials::default(),
            },
            auth: rest.auth(),
            previous: Default::default(),
        }
    }
    fn allocation() -> Value {
        json!({"id":"type-1","siteId":"site-1","infrastructureProvider":{"org":"provider-org"},"allocationStats":{"assigned":200,"total":100,"used":1,"unused":99,"unusedUsable":99}})
    }
    fn seed(rest: &FakeRest) {
        rest.set_total("instance", 1);
        rest.set_pages("instance", vec![vec![instance()]]);
        rest.set_total("machine", 1);
        rest.set_pages("machine", vec![vec![machine(0)]]);
        rest.set_object(
            "tenant/current",
            json!({"id":"tenant-id","org":"tenant-org"}),
        );
        rest.set_object("instance/type/type-1", allocation());
    }

    #[tokio::test]
    async fn complete_paging_and_rejected_evidence() {
        for case in [
            "complete",
            "duplicate",
            "short page",
            "too many",
            "missing labels",
            "null labels",
            "wrong site",
            "changing total",
        ] {
            let rest = FakeRest::start().await;
            let mut rows: Vec<_> = (0..101).map(machine).collect();
            match case {
                "duplicate" => rows[100] = rows[99].clone(),
                "short page" => {
                    rows.pop();
                }
                "missing labels" => {
                    rows[0].as_object_mut().unwrap().remove("labels");
                }
                "null labels" => rows[0]["labels"] = Value::Null,
                "wrong site" => rows[0]["siteId"] = json!("other-site"),
                _ => {}
            }
            rest.set_total(
                "machine",
                if case == "too many" {
                    MAX_ROWS as u64 + 1
                } else {
                    101
                },
            );
            if case == "changing total" {
                rows.push(machine(101));
                rest.set_total("machine?pageNumber=2", 102);
            }
            rest.set_pages("machine", rows.chunks(100).map(|c| c.to_vec()).collect());
            let out = fixture(&rest)
                .machines(
                    &rest.client(),
                    &ObservationRecorder::default(),
                    "site-1",
                    &rest.auth(),
                    "provider-org",
                )
                .await;
            if matches!(case, "complete" | "null labels") {
                let data = out.unwrap_or_else(|e| panic!("{case}: {e:?}"));
                assert_eq!(data.units.len(), 101);
                assert_eq!(
                    data.units[1].rest_machine.as_ref().unwrap().labels["keep"],
                    "old"
                );
                assert!(
                    rest.requests()
                        .iter()
                        .all(|(m, path, _)| m == "GET" && path.contains("/org/provider-org/"))
                );
            } else {
                assert!(out.is_err(), "{case}");
            }
        }
    }

    // The REST API sends null (or "") until the site controller reconciles
    // the instance. One such row must not fail the whole page.
    #[tokio::test]
    async fn unreconciled_instances_publish_without_a_controller_id() {
        let rest = FakeRest::start().await;
        seed(&rest);
        let rows = [("rest-a", json!(null)), ("rest-b", json!(""))].map(|(id, controller)| {
            let mut row = instance();
            row["id"] = json!(id);
            row["controllerInstanceId"] = controller;
            row
        });
        rest.set_total("instance", 2);
        rest.set_pages("instance", vec![rows.to_vec()]);
        let sink = FakeSink::default();
        let attempt = Attempt::new(Reader::RestInventory, &FAMILIES);
        let now = crate::clock::now();
        let meta = SweepMeta {
            process: 1,
            seq: 1,
            reader: Reader::RestInventory,
            start: now,
            end: now,
        };
        attempt.begin();
        let mut failed = Incomplete::default();
        fixture(&rest)
            .read(
                &rest.client(),
                &ObservationRecorder::default(),
                &Inventory {
                    site_id: "site-1".into(),
                    provider_org: "provider-org".into(),
                    provider_auth: rest.auth(),
                    tenant: None,
                    totals: true,
                },
                Publication {
                    meta: &meta,
                    attempt: &attempt,
                    sink: &sink,
                },
                &mut failed,
            )
            .await;
        assert!(failed.finish().is_ok());
        let units = sink.latest::<UnitsData>(Family::RestInstances).unwrap().1;
        assert_eq!(units.units.len(), 2);
        assert!(
            units
                .units
                .iter()
                .all(|u| u.controller_instance_id.is_none())
        );
    }

    // Every family shares its unchanged units with the sweep before, as the
    // nico-api families do, so a large fixture does not reallocate each sweep.
    #[tokio::test]
    async fn unchanged_units_are_shared_with_the_previous_sweep() {
        let rest = FakeRest::start().await;
        seed(&rest);
        let f = fixture(&rest);
        let rec = ObservationRecorder::default();
        let first = f.instances(&rest.client(), &rec, "site-1").await.unwrap();
        let machines = f
            .machines(&rest.client(), &rec, "site-1", &rest.auth(), "provider-org")
            .await
            .unwrap();
        let mut changed = instance();
        changed["status"] = json!("Error");
        rest.set_pages("instance", vec![vec![changed]]);
        let second = f.instances(&rest.client(), &rec, "site-1").await.unwrap();
        assert!(!Arc::ptr_eq(&first.units[0], &second.units[0]));
        let third = f.instances(&rest.client(), &rec, "site-1").await.unwrap();
        assert!(Arc::ptr_eq(&second.units[0], &third.units[0]));
        let again = f
            .machines(&rest.client(), &rec, "site-1", &rest.auth(), "provider-org")
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&machines.units[0], &again.units[0]));
    }

    #[tokio::test]
    async fn instance_names_follow_database_collation_not_byte_order() {
        let rest = FakeRest::start().await;
        let rows = ["aa", "Zz", "éé", "ÅÅ"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                let mut row = instance();
                row["id"] = json!(format!("rest-{i}"));
                row["name"] = json!(name);
                row
            })
            .collect();
        rest.set_pages("instance", vec![rows]);
        let data = fixture(&rest)
            .instances(&rest.client(), &ObservationRecorder::default(), "site-1")
            .await
            .expect("valid collated names");
        assert_eq!(data.units.len(), 4);
        assert!(
            rest.requests()
                .iter()
                .all(|(_, _, query)| query.contains("orderBy=NAME_ASC"))
        );
    }

    #[tokio::test]
    async fn scheduled_timeout_preserves_early_tenant_phase() {
        let rest = FakeRest::start().await;
        seed(&rest);
        rest.set_resource_delay("machine", Duration::from_secs(60));
        let sink = Arc::new(FakeSink::default());
        let reader: Arc<dyn Probe> = Arc::new(RestInventoryReader::new(
            ReaderConfig {
                enabled: true,
                interval: Duration::from_secs(120),
                timeout: Duration::from_secs(2),
            },
            rest.client(),
            Inventory {
                provider_auth: rest.auth(),
                provider_org: "provider-org".into(),
                site_id: "site-1".into(),
                tenant: Some(fixture(&rest)),
                totals: true,
            },
            None,
            sink.clone(),
            std::time::SystemTime::now(),
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        let (mut results, loops) = crate::framework::start(vec![reader.clone()], cancel.clone());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !rest
                .requests()
                .iter()
                .any(|(_, path, _)| path.ends_with("/machine"))
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Machine read started");
        let machine_at = crate::clock::now();
        let early = sink
            .latest::<UnitsData>(Family::RestInstances)
            .expect("early instances");
        assert!(early.0.start < early.0.end && early.0.end <= machine_at);
        assert_eq!(early.1.units[0].id, "rest-id");
        let result = tokio::time::timeout(Duration::from_secs(5), results.recv())
            .await
            .expect("bounded timeout")
            .expect("result");
        assert_eq!(result.outcome, Outcome::Timeout);
        assert_eq!(sink.latest::<UnitsData>(Family::RestInstances), Some(early));
        assert!(sink.failure_kinds(Family::RestInstances).is_empty());
        for family in [
            Family::RestMachines,
            Family::ConfigRest,
            Family::RestTotals,
            Family::RestSiteMatch,
        ] {
            assert_eq!(sink.count(family), 0, "{family:?} was unfinished");
            assert_eq!(
                sink.failure_kinds(family),
                vec![FailureKind::Timeout],
                "{family:?}"
            );
        }
        assert_eq!(sink.failures.lock().unwrap().len(), 4);
        assert_eq!(sink.count(Family::RestInstances), 1);
        cancel.cancel();
        crate::framework::join_loops(loops)
            .await
            .expect("loops end");
    }

    #[tokio::test]
    async fn independent_families_keep_last_good_on_failure() {
        for case in [
            "success",
            "instance failure",
            "machine failure",
            "allocation failure",
        ] {
            let rest = FakeRest::start().await;
            seed(&rest);
            let f = fixture(&rest);
            let sink = FakeSink::default();
            let attempt = Attempt::new(Reader::RestInventory, &FAMILIES);
            let now = crate::clock::now();
            let mut base = SweepMeta {
                process: 1,
                seq: 1,
                reader: Reader::RestInventory,
                start: now,
                end: now,
            };
            attempt.begin();
            let mut failed = Incomplete::default();
            f.read(
                &rest.client(),
                &ObservationRecorder::default(),
                &Inventory {
                    site_id: "site-1".into(),
                    provider_org: "provider-org".into(),
                    provider_auth: rest.auth(),
                    tenant: None,
                    totals: true,
                },
                Publication {
                    meta: &base,
                    attempt: &attempt,
                    sink: &sink,
                },
                &mut failed,
            )
            .await;
            assert!(failed.finish().is_ok());
            let units = sink.latest::<UnitsData>(Family::RestInstances).unwrap().1;
            assert_eq!(units.units[0].id, "rest-id");
            assert_eq!(
                units.units[0].controller_instance_id.as_deref(),
                Some("core-id")
            );
            match case {
                "instance failure" => {
                    let mut r = instance();
                    r["tenantId"] = json!("foreign");
                    rest.set_pages("instance", vec![vec![r]]);
                }
                "machine failure" => rest.set_pages("machine", vec![]),
                "allocation failure" => {
                    let mut a = allocation();
                    a["allocationStats"]["unusedUsable"] = json!(100);
                    rest.set_object("instance/type/type-1", a);
                }
                _ => {}
            }
            base.seq = 2;
            attempt.begin();
            let mut failed = Incomplete::default();
            f.read(
                &rest.client(),
                &ObservationRecorder::default(),
                &Inventory {
                    site_id: "site-1".into(),
                    provider_org: "provider-org".into(),
                    provider_auth: rest.auth(),
                    tenant: None,
                    totals: true,
                },
                Publication {
                    meta: &base,
                    attempt: &attempt,
                    sink: &sink,
                },
                &mut failed,
            )
            .await;
            assert_eq!(failed.finish().is_err(), case != "success");
            let bad = match case {
                "instance failure" => Some(Family::RestInstances),
                "machine failure" => Some(Family::RestMachines),
                "allocation failure" => Some(Family::ConfigRest),
                _ => None,
            };
            for family in FAMILIES {
                assert_eq!(
                    sink.count(family),
                    if Some(family) == bad { 1 } else { 2 },
                    "{case}: {family:?}"
                );
            }
            if let Some(family) = bad {
                assert!(!sink.failure_kinds(family).is_empty());
            }
        }
    }

    // The REST routes are tenant/current and instance/type/{id}. The fake
    // answers any path, so pin the paths and query the probe really sends.
    #[tokio::test]
    async fn allocation_reads_the_real_rest_routes() {
        let rest = FakeRest::start().await;
        seed(&rest);
        fixture(&rest)
            .allocation(
                &rest.client(),
                &ObservationRecorder::default(),
                "site-1",
                "provider-org",
            )
            .await
            .expect("allocation");
        let requests: Vec<_> = rest
            .requests()
            .into_iter()
            .map(|(_, path, query)| (path, query))
            .collect();
        assert_eq!(
            requests,
            [
                (
                    "/v2/org/tenant-org/nico/tenant/current".to_string(),
                    String::new()
                ),
                (
                    "/v2/org/tenant-org/nico/instance/type/type-1".to_string(),
                    "includeAllocationStats=true&includeRelation=InfrastructureProvider"
                        .to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn allocation_rejects_wrong_scope_and_bad_counts() {
        for (field, value) in [
            ("siteId", json!("wrong")),
            ("id", json!("wrong")),
            ("infrastructureProvider", json!({"org":"tenant-org"})),
            (
                "allocationStats",
                json!({"assigned":200,"total":100,"used":-1,"unused":101,"unusedUsable":99}),
            ),
        ] {
            let rest = FakeRest::start().await;
            seed(&rest);
            let mut a = allocation();
            a[field] = value;
            rest.set_object("instance/type/type-1", a);
            assert!(
                fixture(&rest)
                    .allocation(
                        &rest.client(),
                        &ObservationRecorder::default(),
                        "site-1",
                        "provider-org"
                    )
                    .await
                    .is_err(),
                "{field}"
            );
        }
    }
}
