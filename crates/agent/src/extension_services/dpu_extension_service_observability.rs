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

use std::collections::HashSet;

use eyre::WrapErr;
use gtmpl_derive::Gtmpl;
use rpc::errors::RpcDataConversionError;
use rpc::forge as rpc_forge;
use sha2::{Digest, Sha256};
use uuid::Uuid;

// Path to the OTEL config validator.
const OTEL_CONTRIB_VALIDATE_BIN: &str = "/etc/otelcol-contrib/otelcol-wrapper-validate";

const TMPL_OTEL: &str = include_str!("../../templates/dpu_extension_service_observability.tmpl");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpuExtensionServiceObservabilityConfig {
    pub name: Option<String>,
    pub config: DpuExtensionServiceObservabilityConfigType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpuExtensionServiceObservabilityConfigTypePrometheus {
    pub scrape_interval_seconds: u32,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpuExtensionServiceObservabilityConfigTypeLogging {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DpuExtensionServiceObservabilityConfigType {
    Prometheus(DpuExtensionServiceObservabilityConfigTypePrometheus),
    Logging(DpuExtensionServiceObservabilityConfigTypeLogging),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpuExtensionServiceObservability {
    pub configs: Vec<DpuExtensionServiceObservabilityConfig>,
}

//
// Go template objects
//

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplLogging {
    Id: String,
    Name: String,
    Path: String,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplPrometheus {
    Id: String,
    Name: String,
    ScrapeIntervalSeconds: u32,
    Endpoint: String,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplObservability {
    Id: String,
    Name: String,
    Logging: Vec<TmplLogging>,
    Prometheus: Vec<TmplPrometheus>,
}

pub fn build(
    id: Uuid,
    service_name: String,
    observability: &DpuExtensionServiceObservability,
) -> eyre::Result<String> {
    let mut params = TmplObservability {
        Id: format!("{id}"),
        Name: service_name.clone(),
        Logging: vec![],
        Prometheus: vec![],
    };

    // We'll generate our own stable IDs per config, and we can throw
    // away any duplicates in the process.
    let mut unique_configs = HashSet::<String>::new();

    for config in observability.configs.iter() {
        let mut hasher = Sha256::default();
        match config.config {
            DpuExtensionServiceObservabilityConfigType::Prometheus(ref c) => {
                hasher.update(c.endpoint.as_bytes());
                hasher.update(c.scrape_interval_seconds.to_ne_bytes());

                let id = format!("prom-{}", hex::encode_upper(hasher.finalize()));

                if unique_configs.insert(id.clone()) {
                    params.Prometheus.push(TmplPrometheus {
                        Id: id,
                        Name: config.name.clone().unwrap_or(service_name.clone()),
                        Endpoint: c.endpoint.clone(),
                        ScrapeIntervalSeconds: c.scrape_interval_seconds,
                    });
                }
            }
            DpuExtensionServiceObservabilityConfigType::Logging(ref c) => {
                hasher.update(c.path.as_bytes());
                let id = format!("log-{}", hex::encode_upper(hasher.finalize()));

                if unique_configs.insert(id.clone()) {
                    params.Logging.push(TmplLogging {
                        Id: id,
                        Name: config.name.clone().unwrap_or(service_name.clone()),
                        Path: c.path.clone(),
                    });
                }
            }
        }
    }

    gtmpl::template(TMPL_OTEL, params).map_err(|e| {
        println!("ERR filling template: {e}",);
        e.into()
    })
}

// Validate the config
pub async fn validate() -> eyre::Result<bool> {
    let mut cmd = tokio::process::Command::new(OTEL_CONTRIB_VALIDATE_BIN);
    let cmd_str = super::super::pretty_cmd(cmd.as_std());
    tracing::debug!(
        command = cmd_str.as_str(),
        "running otel validation commands"
    );

    // Invoke the validation command and allow a few seconds for completion.
    // The validation should be immediate (under a second), but a few seconds of buffer
    // allows for validation even when the DPU has a bit of load/thread-contention without
    // waiting forever.
    let out = tokio::time::timeout(std::time::Duration::from_secs(3), cmd.output())
        .await
        .wrap_err("timeout running otel validation commands")?
        .wrap_err("error running otel validation commands")?;

    if !out.status.success() {
        tracing::error!(
            command = cmd_str.as_str(),
            stdout = %String::from_utf8_lossy(&out.stdout),
            stderr = %String::from_utf8_lossy(&out.stderr),
            "OTel validation command failed"
        );

        return Ok(false);
    }

    Ok(true)
}

impl TryFrom<rpc_forge::DpuExtensionServiceObservabilityConfig>
    for DpuExtensionServiceObservabilityConfig
{
    type Error = RpcDataConversionError;

    fn try_from(c: rpc_forge::DpuExtensionServiceObservabilityConfig) -> Result<Self, Self::Error> {
        let Some(config) = c.config else {
            return Err(RpcDataConversionError::MissingArgument(
                "DpuExtensionServiceObservability.config",
            ));
        };

        Ok(Self {
            name: c.name,
            config: match config {
                rpc_forge::dpu_extension_service_observability_config::Config::Prometheus(c) => {
                    DpuExtensionServiceObservabilityConfigType::Prometheus(
                        DpuExtensionServiceObservabilityConfigTypePrometheus {
                            scrape_interval_seconds: c.scrape_interval_seconds,
                            endpoint: c.endpoint,
                        },
                    )
                }
                rpc_forge::dpu_extension_service_observability_config::Config::Logging(c) => {
                    DpuExtensionServiceObservabilityConfigType::Logging(
                        DpuExtensionServiceObservabilityConfigTypeLogging { path: c.path },
                    )
                }
            },
        })
    }
}

impl TryFrom<rpc_forge::DpuExtensionServiceObservability> for DpuExtensionServiceObservability {
    type Error = RpcDataConversionError;

    fn try_from(o: rpc_forge::DpuExtensionServiceObservability) -> Result<Self, Self::Error> {
        Ok(Self {
            configs: o
                .configs
                .into_iter()
                .map(DpuExtensionServiceObservabilityConfig::try_from)
                .collect::<Result<Vec<DpuExtensionServiceObservabilityConfig>, _>>()?,
        })
    }
}
