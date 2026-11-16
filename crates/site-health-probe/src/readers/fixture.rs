/* SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0 */
//! Run-owned, REST-only discovery. No tenant/current or arbitrary upstream path.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use carbide_site_health_probe_api::Family;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::rest_instances::{Fixture, pages};
use super::{Attempt, Failure};
use crate::config::{FixtureConfig, ReaderConfig, TenantAccount};
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::CallGate;
use crate::rest::auth::Auth;
use crate::rest::client::RestClient;
use crate::snapshot::{Reader, SnapshotSink, SweepMeta};
use crate::state_metrics::unix_seconds;

pub(crate) struct Catalog {
    pub config: FixtureConfig,
    pub client: Arc<RestClient>,
    pub provider_auth: Arc<Auth>,
    pub tenant_auth: Arc<Auth>,
    pub provider: String,
    pub site: String,
    pub process: SystemTime,
    pub gate: Arc<CallGate>,
    requests: tokio::sync::Mutex<()>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reply {
    kind: String,
    site_id: String,
    org: String,
    process_start_seconds: f64,
    calls_total: u64,
    read_start: chrono::DateTime<chrono::Utc>,
    read_at: chrono::DateTime<chrono::Utc>,
    objects: Vec<Value>,
}
fn invalid() -> Failure {
    Failure::unresolved("fixture discovery identity, scope or specification is incomplete".into())
}
fn id(v: &Value) -> &str {
    v["id"].as_str().unwrap_or("")
}
impl Catalog {
    pub(crate) fn new(
        config: FixtureConfig,
        client: Arc<RestClient>,
        provider_auth: Arc<Auth>,
        tenant_auth: Arc<Auth>,
        scope: (String, String),
        process: SystemTime,
        gate: Arc<CallGate>,
    ) -> Self {
        Self {
            config,
            client,
            provider_auth,
            tenant_auth,
            provider: scope.0,
            site: scope.1,
            process,
            gate,
            requests: tokio::sync::Mutex::new(()),
        }
    }
    pub(crate) async fn objects(
        &self,
        kind: &str,
        rec: &ObservationRecorder,
    ) -> Result<Vec<Value>, Failure> {
        let (path, suffix, provider) = match kind {
            "account" => ("tenant/account", "", true),
            "type" => ("instance/type", "type", true),
            "compute" => ("allocation", "compute", true),
            "ipallocation" => ("allocation", "ipallocation", true),
            "ipblock" => ("ipblock", "ipblock", true),
            "vpc" => ("vpc", "vpc", false),
            "subnet" => ("subnet", "subnet", false),
            "os" => ("operating-system", "os", false),
            "blocks" => ("ipblock", "", false),
            "providerBlocks" => ("ipblock", "", true),
            "provider" => {
                let v: Value = self
                    .client
                    .get(
                        rec,
                        "fixture_provider",
                        &self.provider_auth,
                        &self.provider,
                        &["site", &self.site],
                        &[("includeRelation", "InfrastructureProvider".into())],
                    )
                    .await
                    .map_err(|e| Failure::call("fixture_provider", &e))?
                    .json()
                    .map_err(|e| Failure::call("fixture_provider", &e))?;
                let provider = &v["infrastructureProvider"];
                // The REST provider summary omits its ID, so read it from the site.
                let provider_id = v["infrastructureProviderId"].as_str().unwrap_or_default();
                if id(&v) != self.site
                    || provider_id.is_empty()
                    || !provider["id"].is_null()
                        && provider["id"] != ""
                        && provider["id"] != provider_id
                    || provider["org"] != self.provider
                {
                    return Err(invalid());
                }
                return Ok(vec![json!({"id":provider_id,"org":self.provider})]);
            }
            _ => return Err(invalid()),
        };
        let auth = if provider {
            &self.provider_auth
        } else {
            &self.tenant_auth
        };
        let org = if provider {
            &self.provider
        } else {
            &self.config.tenant_org
        };
        let rows = pages::<Value>(&self.client, rec, auth, org, &self.site, path, id).await?;
        let name = format!("{}-{suffix}", self.config.name);
        let matches: Vec<_> = rows
            .into_iter()
            .filter(|v| {
                if matches!(kind, "blocks" | "providerBlocks") {
                    true
                } else if kind == "account" {
                    v["tenantOrg"] == self.config.tenant_org
                } else {
                    v["name"] == name
                }
            })
            .collect();
        if !matches!(kind, "blocks" | "providerBlocks") && matches.len() > 1 {
            return Err(invalid());
        }
        matches
            .into_iter()
            .map(|v| normalize(v, &self.site))
            .collect()
    }
    async fn lookup(&self, kind: &str) -> Result<Reply, Failure> {
        let _permit = self.requests.lock().await;
        let start = crate::clock::now();
        let objects = if kind == "status" {
            vec![]
        } else {
            self.objects(kind, &ObservationRecorder::default()).await?
        };
        Ok(Reply {
            kind: kind.into(),
            site_id: self.site.clone(),
            org: self.config.tenant_org.clone(),
            process_start_seconds: unix_seconds(self.process),
            calls_total: self.gate.total_calls(),
            read_start: start.into(),
            read_at: crate::clock::now().into(),
            objects,
        })
    }
}
// Only normalized specifications are public. Never forward statusHistory,
// upstream errors, arbitrary metadata, credentials or the OS script itself.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountCapability {
    site_ids: Vec<String>,
    targeted_instance_creation: bool,
}
fn normalize(v: Value, site: &str) -> Result<Value, Failure> {
    if id(&v).is_empty()
        || v.get("siteId")
            .and_then(Value::as_str)
            .is_some_and(|s| s != site)
    {
        return Err(invalid());
    }
    let status = v["status"].as_str().unwrap_or("");
    if ![
        "",
        "Ready",
        "Registered",
        "Pending",
        "Invited",
        "Registering",
        "Syncing",
        "Deactivated",
        "Deleted",
        "Unknown",
        "Error",
        "Deleting",
        "Provisioning",
    ]
    .contains(&status)
    {
        return Err(invalid());
    }
    let mut out = serde_json::Map::new();
    for key in [
        "id",
        "name",
        "status",
        "org",
        "siteId",
        "tenantId",
        "tenantOrg",
        "infrastructureProviderId",
        "vpcId",
        "ipv4BlockId",
        "prefix",
        "prefixLength",
        "routingType",
        "protocolVersion",
        "networkVirtualizationType",
        "phoneHomeEnabled",
        "ipv4Prefix",
    ] {
        if let Some(value) = v.get(key) {
            if value.is_object()
                || value.is_array()
                || value.as_str().is_some_and(|s| s.len() > 512)
            {
                return Err(invalid());
            }
            out.insert(key.into(), value.clone());
        }
    }
    if let Some(raw) = v.get("siteCapabilities") {
        let caps: Vec<AccountCapability> =
            serde_json::from_value(raw.clone()).map_err(|_| invalid())?;
        let defaults: Vec<_> = caps.iter().filter(|c| c.site_ids.is_empty()).collect();
        if defaults.len() != 1 {
            return Err(invalid());
        }
        let mut effective = defaults[0].targeted_instance_creation;
        let mut seen = std::collections::BTreeSet::new();
        for cap in &caps {
            for id in &cap.site_ids {
                if id.is_empty() || !seen.insert(id) {
                    return Err(invalid());
                }
                if id == site {
                    effective = cap.targeted_instance_creation;
                }
            }
        }
        out.insert("targetedInstanceCreation".into(), json!(effective));
        out.insert(
            "siteCapabilities".into(),
            serde_json::to_value(caps).map_err(|_| invalid())?,
        );
    }
    if let Some(caps) = v.get("machineCapabilities") {
        let caps: Vec<carbide_site_health_probe_api::MachineCapability> =
            serde_json::from_value(caps.clone()).map_err(|_| invalid())?;
        out.insert(
            "machineCapabilities".into(),
            serde_json::to_value(caps).map_err(|_| invalid())?,
        );
    }
    if let Some(constraints) = v["allocationConstraints"].as_array() {
        if constraints.len() > 16 {
            return Err(invalid());
        }
        let mut normalized = Vec::new();
        for c in constraints {
            let mut row = serde_json::Map::new();
            for key in [
                "id",
                "resourceType",
                "resourceTypeId",
                "constraintType",
                "constraintValue",
                "derivedResourceId",
            ] {
                if let Some(value) = c.get(key) {
                    if value.is_object() || value.is_array() {
                        return Err(invalid());
                    }
                    row.insert(key.into(), value.clone());
                }
            }
            normalized.push(Value::Object(row));
        }
        out.insert("allocationConstraints".into(), Value::Array(normalized));
    }
    if let Some(script) = v["ipxeScript"].as_str() {
        out.insert(
            "ipxeScriptHash".into(),
            Value::String(
                Sha256::digest(script.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
            ),
        );
    }
    Ok(Value::Object(out))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    kind: String,
}
async fn lookup(
    State(c): State<Arc<Catalog>>,
    Query(q): Query<Lookup>,
) -> Result<Json<Reply>, (StatusCode, Json<Value>)> {
    match tokio::time::timeout(Duration::from_secs(30), c.lookup(&q.kind)).await {
        Ok(Ok(reply)) => Ok(Json(reply)),
        _ => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"fixture discovery unavailable"})),
        )),
    }
}
pub(crate) fn router(catalog: Arc<Catalog>) -> Router {
    Router::new()
        .route("/v1/fixture", get(lookup))
        .with_state(catalog)
}

pub(crate) struct FixtureReader {
    pub catalog: Arc<Catalog>,
    pub cfg: ReaderConfig,
    pub sink: Arc<dyn SnapshotSink>,
    seq: AtomicU64,
    attempt: Attempt,
}
impl FixtureReader {
    pub(crate) fn new(
        catalog: Arc<Catalog>,
        cfg: ReaderConfig,
        sink: Arc<dyn SnapshotSink>,
    ) -> Self {
        Self {
            catalog,
            cfg,
            sink,
            seq: AtomicU64::new(0),
            attempt: Attempt::new(Reader::RestInventory, &super::rest_instances::FAMILIES),
        }
    }
    fn deliver<T: Send + Sync + 'static>(
        &self,
        seq: u64,
        start: SystemTime,
        family: Family,
        result: Result<T, Failure>,
    ) -> bool {
        self.attempt.deliver(
            self.sink.as_ref(),
            &SweepMeta {
                process: crate::snapshot::unix_millis(self.catalog.process),
                seq,
                reader: Reader::RestInventory,
                start,
                end: crate::clock::now(),
            },
            family,
            result,
        )
    }
}
#[async_trait::async_trait]
impl Probe for FixtureReader {
    fn name(&self) -> &'static str {
        "rest_fixture"
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
    async fn run(&self, rec: &ObservationRecorder) -> eyre::Result<()> {
        self.attempt.begin();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let c = &self.catalog;
        let mut fixture = Fixture {
            config: TenantAccount {
                org: c.config.tenant_org.clone(),
                tenant_id: String::new(),
                instance_type: String::new(),
                auth: c.config.auth.clone(),
            },
            auth: c.tenant_auth.clone(),
            previous: Default::default(),
        };
        let start = crate::clock::now();
        let machines = fixture
            .machines(&c.client, rec, &c.site, &c.provider_auth, &c.provider)
            .await;
        let mut good = self.deliver(seq, start, Family::RestMachines, machines);
        let identity = async {
            let accounts = c.objects("account", rec).await?;
            if accounts.len() != 1 {
                return Err(invalid());
            }
            let tenant = accounts[0]["tenantId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(invalid)?;
            Ok(tenant.to_string())
        }
        .await;
        match identity {
            Ok(tenant) => {
                fixture.config.tenant_id = tenant;
                let start = crate::clock::now();
                let instances = fixture.instances(&c.client, rec, &c.site).await;
                good &= self.deliver(seq, start, Family::RestInstances, instances);
                let start = crate::clock::now();
                let allocation = async {
                    let types = c.objects("type", rec).await?;
                    if types.len() != 1 {
                        return Err(invalid());
                    }
                    fixture.config.instance_type = id(&types[0]).to_string();
                    fixture
                        .allocation_for_site(&c.client, rec, &c.site, &c.provider)
                        .await
                }
                .await;
                good &= self.deliver(seq, start, Family::ConfigRest, allocation);
            }
            Err(error) => {
                for family in [Family::RestInstances, Family::ConfigRest] {
                    good &=
                        self.deliver::<()>(seq, crate::clock::now(), family, Err(error.clone()));
                }
            }
        }
        if good {
            Ok(())
        } else {
            Err(eyre::eyre!("fixture families incomplete"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RestCredentials;
    use crate::framework::Outcome;
    use crate::rest::fake::FakeRest;
    use crate::snapshot::UnitsData;
    use crate::snapshot::testing::FakeSink;

    #[test]
    fn catalog_specs_are_scoped_and_redacted() {
        for (case, value, valid) in [
            (
                "script",
                json!({"id":"os","siteId":"site","ipxeScript":"secret-script","statusHistory":["secret"],"labels":{"secret":"secret"}}),
                true,
            ),
            ("foreign", json!({"id":"x","siteId":"other"}), false),
            ("missing", json!({"name":"x"}), false),
            (
                "unknown status",
                json!({"id":"x","status":"unknown"}),
                false,
            ),
            (
                "nullable count",
                json!({"id":"x","machineCapabilities":[{"type":"GPU","name":"gpu","count":null}]}),
                true,
            ),
        ] {
            let out = normalize(value, "site");
            assert_eq!(out.is_ok(), valid, "{case}");
            if case == "script" {
                let text = out.unwrap().to_string();
                assert!(!text.contains("secret"));
                assert!(text.contains("ipxeScriptHash"));
            }
        }
    }

    #[test]
    fn fixture_statuses_follow_rest_models() {
        for status in [
            "Pending",
            "Provisioning",
            "Ready",
            "Error",
            "Deleting",
            "Syncing",
            "Deactivated",
            "Registering",
            "Registered",
            "Invited",
            "Deleted",
            "Unknown",
        ] {
            let row = normalize(
                json!({"id":"resource","siteId":"site","status":status}),
                "site",
            )
            .unwrap();
            assert_eq!(row["status"], status);
        }
        assert!(normalize(json!({"id":"resource","status":"future-status"}), "site").is_err());
    }

    #[test]
    fn subnet_preserves_rest_prefix_length() {
        for status in ["Pending", "Ready"] {
            let out = normalize(json!({"id":"subnet","siteId":"site","vpcId":"vpc","ipv4BlockId":"block","ipv4Prefix":"10.210.0.0","prefixLength":24,"status":status}), "site").unwrap();
            assert_eq!(out["prefixLength"], 24);
            assert_eq!(out["status"], status);
        }
    }

    #[test]
    fn account_site_capabilities_preserve_defaults_and_overrides() {
        for (caps, expected) in [
            (
                json!([{"siteIds":[],"targetedInstanceCreation":false}]),
                Some(false),
            ),
            (
                json!([{"siteIds":[],"targetedInstanceCreation":true},{"siteIds":["site"],"targetedInstanceCreation":false}]),
                Some(false),
            ),
            (
                json!([{"siteIds":[],"targetedInstanceCreation":false},{"siteIds":["site","other"],"targetedInstanceCreation":true}]),
                Some(true),
            ),
            (json!([]), None),
            (
                json!([{"siteIds":[],"targetedInstanceCreation":false},{"siteIds":[],"targetedInstanceCreation":true}]),
                None,
            ),
            (
                json!([{"siteIds":[],"targetedInstanceCreation":false},{"siteIds":["site","site"],"targetedInstanceCreation":true}]),
                None,
            ),
        ] {
            let result = normalize(json!({"id":"account","siteCapabilities":caps}), "site");
            if let Some(enabled) = expected {
                let result = result.unwrap();
                assert_eq!(result["targetedInstanceCreation"], enabled);
                assert_eq!(result["siteCapabilities"], caps);
            } else {
                assert!(result.is_err());
            }
        }
    }

    #[tokio::test]
    async fn provider_publication_precedes_missing_or_stalled_tenant() {
        for (stalled, tenant) in [(false, false), (true, false), (false, true)] {
            let rest = FakeRest::start().await;
            rest.set_object("site/site-1", json!({"id":"site-1","infrastructureProviderId":"provider-id","infrastructureProvider":{"id":null,"org":"provider-org"}}));
            rest.set_total("ipblock", 2);
            rest.set_pages("ipblock", vec![vec![json!({"id":"root-1","name":"fabric-one","siteId":"site-1","infrastructureProviderId":"provider-id","prefix":"10.1.0.0","prefixLength":16}),json!({"id":"root-2","name":"fabric-two","siteId":"site-1","infrastructureProviderId":"provider-id","prefix":"10.2.0.0","prefixLength":16})]]);
            rest.set_total("machine", 1);
            rest.set_pages("machine", vec![vec![json!({"id":"fm100htopaque","controllerMachineId":"fm100htopaque","siteId":"site-1","instanceTypeId":null,"instanceId":null,"tenantId":null,"labels":null,"machineCapabilities":null,"isUsableByTenant":false,"status":"Ready"})]]);
            rest.set_total("tenant/account", 0);
            rest.set_pages("tenant/account", vec![vec![]]);
            rest.set_total("instance/type", 0);
            rest.set_pages("instance/type", vec![vec![]]);
            if tenant {
                rest.set_total("tenant/account", 1);
                rest.set_pages("tenant/account", vec![vec![json!({"id":"account","infrastructureProviderId":"provider-id","tenantOrg":"tenant-org","tenantId":"tenant-id"})]]);
                rest.set_total("instance", 0);
                rest.set_pages("instance", vec![vec![]]);
            }
            if stalled {
                rest.set_resource_delay("tenant/account", Duration::from_secs(60));
            }
            let catalog = Arc::new(Catalog::new(
                FixtureConfig {
                    name: "test".into(),
                    tenant_org: "tenant-org".into(),
                    auth: RestCredentials::default(),
                },
                rest.client(),
                rest.auth(),
                rest.auth(),
                ("provider-org".into(), "site-1".into()),
                SystemTime::now(),
                Arc::new(CallGate::new(
                    "nico-rest-api",
                    100.,
                    Duration::from_secs(5),
                    Duration::from_secs(30),
                )),
            ));
            assert!(catalog.lookup("arbitrary/path").await.is_err());
            let blocks = catalog.lookup("providerBlocks").await.unwrap();
            assert_eq!(blocks.objects.len(), 2);
            assert!(
                rest.requests()
                    .iter()
                    .any(|(method, path, _)| method == "GET"
                        && path == "/v2/org/provider-org/nico/ipblock")
            );
            assert_eq!(
                catalog.lookup("provider").await.unwrap().objects,
                vec![json!({"id":"provider-id","org":"provider-org"})]
            );
            assert!(
                rest.requests()
                    .iter()
                    .any(|(method, path, query)| method == "GET"
                        && path.ends_with("/site/site-1")
                        && query.contains("includeRelation=InfrastructureProvider"))
            );
            for relation_id in [json!("other"), json!("provider-id"), json!(""), Value::Null] {
                rest.set_object("site/site-1", json!({"id":"site-1","infrastructureProviderId":"provider-id","infrastructureProvider":{"id":relation_id,"org":"provider-org"}}));
                assert_eq!(
                    catalog.lookup("provider").await.is_ok(),
                    relation_id != "other"
                );
            }
            let sink = Arc::new(FakeSink::default());
            let reader = Arc::new(FixtureReader::new(
                catalog,
                ReaderConfig {
                    enabled: true,
                    interval: Duration::from_secs(120),
                    timeout: Duration::from_secs(1),
                },
                sink.clone(),
            ));
            if stalled {
                let cancel = tokio_util::sync::CancellationToken::new();
                let (mut results, loops) = crate::framework::start(vec![reader], cancel.clone());
                let result = tokio::time::timeout(Duration::from_secs(5), results.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(result.outcome, Outcome::Timeout);
                cancel.cancel();
                crate::framework::join_loops(loops).await.unwrap();
            } else {
                assert!(reader.run(&ObservationRecorder::default()).await.is_err());
            }
            let (meta, data) = sink
                .latest::<UnitsData>(Family::RestMachines)
                .expect("provider machines published");
            assert_eq!(data.units[0].id, "fm100htopaque");
            assert!(meta.start <= meta.end);
            assert!(sink.failure_kinds(Family::RestMachines).is_empty());
            for family in [Family::RestInstances, Family::ConfigRest] {
                let published = tenant && family == Family::RestInstances;
                assert_eq!(sink.count(family), if published { 1 } else { 0 });
                assert_eq!(sink.failure_kinds(family).is_empty(), published);
            }
            assert!(rest.requests().iter().all(|(_, path, _)| {
                ![
                    "/tenant/current",
                    "/infrastructure-provider/current",
                    "/rack",
                    "/tray",
                ]
                .iter()
                .any(|suffix| path.ends_with(suffix))
            }));
        }
    }
}
