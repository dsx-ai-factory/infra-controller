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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use carbide_uuid::rack::RackId;
use futures::TryStreamExt;
use mac_address::MacAddress;
use nv_redfish::bmc_http::reqwest::Client as ReqwestClient;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgRow};
use sqlx::{Column, Connection, Row};
use url::Url;

use crate::HealthError;
use crate::bmc::{
    BmcClient, BmcLatencyInstrumentation, CredentialProvider, bmc_latency_endpoint_labels,
};
use crate::config::{SqlEndpointSourceConfig, endpoint_label_name_problem};
use crate::endpoint::{
    BmcAddr, BmcEndpoint, BoxFuture, EndpointMetadata, EndpointSource, MachineData,
    SharedSystemUuid,
};
use crate::metrics::BmcLatencyMetrics;

const HOSTNAME: &str = "hostname";
const BMC_IP: &str = "bmc_ip";
const BMC_MAC: &str = "bmc_mac";
const BMC_PORT: &str = "bmc_port";
const RACK_ID: &str = "rack_id";
const SERIAL: &str = "serial";
const FIELD_COLUMNS: &[&str] = &[HOSTNAME, BMC_IP, BMC_MAC, BMC_PORT, RACK_ID, SERIAL];
const MAX_LABEL_COLUMNS: usize = 32;
const MAX_LABEL_VALUE_BYTES: usize = 1024;

#[derive(Debug, PartialEq)]
struct SqlBmcRow {
    addr: BmcAddr,
    hostname: Option<String>,
    rack_id: Option<String>,
    serial: Option<String>,
    labels: BTreeMap<String, String>,
}

#[derive(Debug, PartialEq, thiserror::Error)]
enum RowError {
    #[error("bmc_ip is NULL")]
    MissingIp,
    #[error("invalid bmc_ip {0:?}")]
    InvalidIp(String),
    #[error("invalid bmc_mac {0:?}")]
    InvalidMac(String),
    #[error("invalid bmc_port {0:?}")]
    InvalidPort(String),
    #[error("label {0} exceeds {MAX_LABEL_VALUE_BYTES} bytes")]
    LabelTooLong(String),
}

/// Rejects result columns that cannot map onto an endpoint.
fn check_columns<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut label_count = 0;
    for name in names {
        if !seen.insert(name) {
            return Err(format!("column {name:?} appears more than once"));
        }
        if FIELD_COLUMNS.contains(&name) {
            continue;
        }
        if let Some(problem) = endpoint_label_name_problem(name) {
            return Err(format!("label column {name:?} {problem}"));
        }
        label_count += 1;
    }
    if !seen.contains(BMC_IP) {
        return Err(format!("query must return a {BMC_IP} column"));
    }
    if label_count > MAX_LABEL_COLUMNS {
        return Err(format!(
            "query returns {label_count} label columns; at most {MAX_LABEL_COLUMNS} are supported"
        ));
    }
    Ok(())
}

impl SqlBmcRow {
    /// Parses one row of text values; NULL and empty values count as absent.
    fn parse<'a>(
        values: impl IntoIterator<Item = (&'a str, Option<String>)>,
        default_port: Option<u16>,
    ) -> Result<Self, RowError> {
        let mut ip = None;
        let mut mac = None;
        let mut port = default_port;
        let mut hostname = None;
        let mut rack_id = None;
        let mut serial = None;
        let mut labels = BTreeMap::new();

        for (name, value) in values {
            let Some(value) = value.filter(|value| !value.is_empty()) else {
                continue;
            };
            match name {
                BMC_IP => {
                    ip = Some(IpAddr::from_str(&value).map_err(|_| RowError::InvalidIp(value))?)
                }
                BMC_MAC => {
                    mac = Some(
                        MacAddress::from_str(&value).map_err(|_| RowError::InvalidMac(value))?,
                    )
                }
                BMC_PORT => port = Some(value.parse().map_err(|_| RowError::InvalidPort(value))?),
                HOSTNAME => hostname = Some(value),
                RACK_ID => rack_id = Some(value),
                SERIAL => serial = Some(value),
                label => {
                    if value.len() > MAX_LABEL_VALUE_BYTES {
                        return Err(RowError::LabelTooLong(label.to_owned()));
                    }
                    labels.insert(label.to_owned(), value);
                }
            }
        }

        Ok(Self {
            addr: BmcAddr {
                ip: ip.ok_or(RowError::MissingIp)?,
                port,
                mac,
            },
            hostname,
            rack_id,
            serial,
            labels,
        })
    }
}

struct CachedClient {
    addr: BmcAddr,
    rack_id: Option<RackId>,
    bmc: Arc<BmcClient>,
    system_uuid: SharedSystemUuid,
}

pub(crate) struct SqlEndpointSource {
    cfg: SqlEndpointSourceConfig,
    credentials: Arc<dyn CredentialProvider>,
    reqwest: ReqwestClient,
    proxy_url: Option<Url>,
    cache_size: usize,
    bmc_request_concurrency: NonZeroUsize,
    bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    clients: Mutex<HashMap<String, CachedClient>>,
}

impl SqlEndpointSource {
    pub(crate) fn new(
        cfg: SqlEndpointSourceConfig,
        credentials: Arc<dyn CredentialProvider>,
        reqwest: &ReqwestClient,
        proxy_url: Option<&Url>,
        cache_size: usize,
        bmc_request_concurrency: NonZeroUsize,
        bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    ) -> Self {
        Self {
            cfg,
            credentials,
            reqwest: reqwest.clone(),
            proxy_url: proxy_url.cloned(),
            cache_size,
            bmc_request_concurrency,
            bmc_latency_metrics,
            clients: Mutex::new(HashMap::new()),
        }
    }

    async fn query_rows(&self) -> Result<Vec<SqlBmcRow>, HealthError> {
        let path = &self.cfg.database_url_file;
        let url = tokio::fs::read_to_string(path).await.map_err(|error| {
            HealthError::GenericError(format!(
                "failed to read SQL database URL file {}: {error}",
                path.display()
            ))
        })?;
        // The parse error can echo the URL, which holds the password.
        let options = PgConnectOptions::from_str(url.trim())
            .map_err(|_| {
                HealthError::GenericError(format!(
                    "{} does not hold a valid PostgreSQL URL",
                    path.display()
                ))
            })?
            .application_name("nico-health");

        let mut connection = PgConnection::connect_with(&options)
            .await
            .map_err(HealthError::SqlInventory)?;
        let rows = self.stream_rows(&mut connection).await;
        if let Err(error) = connection.close().await {
            tracing::debug!(%error, "Failed to close SQL inventory connection");
        }
        rows
    }

    async fn stream_rows(
        &self,
        connection: &mut PgConnection,
    ) -> Result<Vec<SqlBmcRow>, HealthError> {
        let mut stream = sqlx::query(sqlx::AssertSqlSafe(self.cfg.query.as_str()))
            .persistent(false)
            .fetch(connection);
        let mut rows = Vec::new();
        let mut columns_checked = false;
        while let Some(row) = stream.try_next().await.map_err(HealthError::SqlInventory)? {
            if !columns_checked {
                check_columns(row.columns().iter().map(Column::name))
                    .map_err(HealthError::GenericError)?;
                columns_checked = true;
            }
            match SqlBmcRow::parse(text_values(&row)?, self.cfg.port) {
                Ok(row) => rows.push(row),
                Err(error) => tracing::warn!(%error, "Skipping SQL inventory row"),
            }
        }
        Ok(rows)
    }

    fn endpoint(&self, row: SqlBmcRow) -> Option<Arc<BmcEndpoint>> {
        let rack_id = row.rack_id.as_deref().map(RackId::new);
        let mut clients = self.clients.lock().expect("client cache mutex poisoned");
        let cache_key = row.addr.registry_key();
        let reusable = clients
            .get(&cache_key)
            .is_some_and(|cached| cached.addr == row.addr && cached.rack_id == rack_id);
        if !reusable {
            let instrumentation = self.bmc_latency_metrics.clone().map(|metrics| {
                BmcLatencyInstrumentation::new(
                    metrics,
                    bmc_latency_endpoint_labels(None, rack_id.as_ref()),
                )
            });
            let bmc = match BmcClient::new(
                self.reqwest.clone(),
                row.addr.clone(),
                self.credentials.clone(),
                self.proxy_url.clone(),
                self.cache_size,
                self.bmc_request_concurrency,
                instrumentation,
            ) {
                Ok(bmc) => Arc::new(bmc),
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        bmc_address = ?row.addr,
                        rack_id = rack_id.as_ref().map(tracing::field::display),
                        "Failed to construct BmcClient for SQL endpoint; skipping"
                    );
                    return None;
                }
            };
            clients.insert(
                cache_key.clone(),
                CachedClient {
                    addr: row.addr.clone(),
                    rack_id: rack_id.clone(),
                    bmc,
                    system_uuid: SharedSystemUuid::default(),
                },
            );
        }
        let cached = clients.get(&cache_key)?;

        Some(Arc::new(BmcEndpoint {
            addr: row.addr,
            hostname: row.hostname,
            identity: None,
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: None,
                machine_serial: row.serial,
                system_uuid: cached.system_uuid.clone(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id,
            labels: row.labels,
            bmc: cached.bmc.clone(),
        }))
    }

    async fn load_endpoints(&self) -> Result<Vec<Arc<BmcEndpoint>>, HealthError> {
        let rows = tokio::time::timeout(self.cfg.query_timeout, self.query_rows())
            .await
            .map_err(|_| {
                HealthError::GenericError(format!(
                    "SQL inventory read exceeded {}",
                    humantime::format_duration(self.cfg.query_timeout)
                ))
            })??;

        let endpoints = self.endpoints_from_rows(rows);
        tracing::info!(endpoint_count = endpoints.len(), "Loaded SQL endpoints");
        Ok(endpoints)
    }

    /// Builds endpoints, reusing cached clients and dropping clients for BMCs
    /// that are no longer returned.
    fn endpoints_from_rows(&self, rows: Vec<SqlBmcRow>) -> Vec<Arc<BmcEndpoint>> {
        let live_keys: HashSet<String> = rows.iter().map(|row| row.addr.registry_key()).collect();
        let endpoints = rows
            .into_iter()
            .filter_map(|row| self.endpoint(row))
            .collect();
        self.clients
            .lock()
            .expect("client cache mutex poisoned")
            .retain(|key, _| live_keys.contains(key));
        endpoints
    }
}

fn text_values(row: &PgRow) -> Result<Vec<(&str, Option<String>)>, HealthError> {
    row.columns()
        .iter()
        .map(|column| {
            row.try_get::<Option<String>, _>(column.ordinal())
                .map(|value| (column.name(), value))
                .map_err(HealthError::SqlInventory)
        })
        .collect()
}

impl EndpointSource for SqlEndpointSource {
    fn fetch_bmc_hosts<'a>(&'a self) -> BoxFuture<'a, Result<Vec<Arc<BmcEndpoint>>, HealthError>> {
        Box::pin(self.load_endpoints())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, FailsWith, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};

    use super::*;
    use crate::bmc::FixedCredentialProvider;
    use crate::endpoint::BmcCredentials;
    use crate::endpoint::test_support::reqwest;

    fn row(values: &[(&'static str, Option<&str>)]) -> Vec<(&'static str, Option<String>)> {
        values
            .iter()
            .map(|(name, value)| (*name, value.map(str::to_owned)))
            .collect()
    }

    fn addr(ip: &str, port: Option<u16>, mac: Option<&str>) -> BmcAddr {
        BmcAddr {
            ip: ip.parse().unwrap(),
            port,
            mac: mac.map(|mac| mac.parse().unwrap()),
        }
    }

    #[test]
    fn parse_maps_columns_onto_endpoint_fields() {
        check_cases(
            [
                Case {
                    scenario: "all fields, labels, and port column over default",
                    input: row(&[
                        (BMC_IP, Some("10.0.0.1")),
                        (BMC_MAC, Some("00:11:22:33:44:55")),
                        (BMC_PORT, Some("8443")),
                        (HOSTNAME, Some("bmc-01")),
                        (RACK_ID, Some("R12")),
                        (SERIAL, Some("SN1")),
                        ("server_type", Some("giga_gb300")),
                    ]),
                    expect: Yields(SqlBmcRow {
                        addr: addr("10.0.0.1", Some(8443), Some("00:11:22:33:44:55")),
                        hostname: Some("bmc-01".into()),
                        rack_id: Some("R12".into()),
                        serial: Some("SN1".into()),
                        labels: BTreeMap::from([("server_type".into(), "giga_gb300".into())]),
                    }),
                },
                Case {
                    scenario: "NULL and empty values are absent; default port applies",
                    input: row(&[
                        (BMC_IP, Some("fd00::1")),
                        (BMC_MAC, None),
                        (HOSTNAME, Some("")),
                        ("server_type", None),
                    ]),
                    expect: Yields(SqlBmcRow {
                        addr: addr("fd00::1", Some(443), None),
                        hostname: None,
                        rack_id: None,
                        serial: None,
                        labels: BTreeMap::new(),
                    }),
                },
                Case {
                    scenario: "NULL bmc_ip",
                    input: row(&[(BMC_IP, None)]),
                    expect: FailsWith(RowError::MissingIp),
                },
                Case {
                    scenario: "invalid bmc_ip",
                    input: row(&[(BMC_IP, Some("10.0.0.1/24"))]),
                    expect: FailsWith(RowError::InvalidIp("10.0.0.1/24".into())),
                },
                Case {
                    scenario: "invalid bmc_mac",
                    input: row(&[(BMC_IP, Some("10.0.0.1")), (BMC_MAC, Some("nope"))]),
                    expect: FailsWith(RowError::InvalidMac("nope".into())),
                },
                Case {
                    scenario: "out-of-range bmc_port",
                    input: row(&[(BMC_IP, Some("10.0.0.1")), (BMC_PORT, Some("70000"))]),
                    expect: FailsWith(RowError::InvalidPort("70000".into())),
                },
                Case {
                    scenario: "oversized label value",
                    input: vec![
                        (BMC_IP, Some("10.0.0.1".to_string())),
                        ("note", Some("x".repeat(MAX_LABEL_VALUE_BYTES + 1))),
                    ],
                    expect: FailsWith(RowError::LabelTooLong("note".into())),
                },
            ],
            |values| SqlBmcRow::parse(values, Some(443)),
        );
    }

    #[test]
    fn check_columns_requires_bmc_ip_and_valid_labels() {
        let too_many: Vec<String> = (0..=MAX_LABEL_COLUMNS).map(|i| format!("l{i}")).collect();
        let too_many: Vec<&str> = std::iter::once(BMC_IP)
            .chain(too_many.iter().map(String::as_str))
            .collect();
        check_cases(
            [
                Case {
                    scenario: "fields and labels",
                    input: vec![BMC_IP, HOSTNAME, "server_type"],
                    expect: Yields(()),
                },
                Case {
                    scenario: "missing bmc_ip",
                    input: vec![HOSTNAME],
                    expect: Fails,
                },
                Case {
                    scenario: "duplicate column",
                    input: vec![BMC_IP, BMC_IP],
                    expect: Fails,
                },
                Case {
                    scenario: "reserved label",
                    input: vec![BMC_IP, "machine_id"],
                    expect: Fails,
                },
                Case {
                    scenario: "invalid label name",
                    input: vec![BMC_IP, "?column?"],
                    expect: Fails,
                },
                Case {
                    scenario: "too many labels",
                    input: too_many,
                    expect: Fails,
                },
            ],
            |names| check_columns(names).map_err(drop),
        );
    }

    #[test]
    fn rows_reuse_clients_until_address_or_rack_changes() {
        let source = SqlEndpointSource::new(
            SqlEndpointSourceConfig::default(),
            Arc::new(FixedCredentialProvider::new(
                BmcCredentials::UsernamePassword {
                    username: "admin".into(),
                    password: None,
                },
            )),
            &reqwest(),
            None,
            10,
            NonZeroUsize::MIN,
            None,
        );
        let load = |ip: &str, rack: Option<&str>| {
            let rack = rack.map(str::to_owned);
            let rows = vec![SqlBmcRow {
                addr: addr(ip, None, Some("00:11:22:33:44:55")),
                hostname: None,
                rack_id: rack,
                serial: None,
                labels: BTreeMap::new(),
            }];
            source.endpoints_from_rows(rows).remove(0)
        };

        let first = load("10.0.0.1", Some("R1"));
        check_values(
            [
                Check {
                    scenario: "same address and rack",
                    input: ("10.0.0.1", Some("R1")),
                    expect: true,
                },
                Check {
                    scenario: "rack changed",
                    input: ("10.0.0.1", Some("R2")),
                    expect: false,
                },
                Check {
                    scenario: "address changed",
                    input: ("10.0.0.2", Some("R2")),
                    expect: false,
                },
            ],
            |(ip, rack)| {
                let previous = source
                    .clients
                    .lock()
                    .unwrap()
                    .values()
                    .next()
                    .unwrap()
                    .bmc
                    .clone();
                Arc::ptr_eq(&previous, &load(ip, rack).bmc)
            },
        );
        assert!(matches!(first.metadata, Some(EndpointMetadata::Machine(_))));

        source.endpoints_from_rows(Vec::new());
        assert!(source.clients.lock().unwrap().is_empty());
    }
}
