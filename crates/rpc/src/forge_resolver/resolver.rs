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
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use hickory_resolver::config::{NameServerConfig, ResolverOpts};
use hickory_resolver::proto::rr::Name;

use crate::forge_resolver::read_resolv_conf;

const RESOLV_CONF_PATH: &str = "/etc/resolv.conf";

#[derive(Clone, Default)]
pub struct ForgeResolverConfig {
    pub inner: Vec<NameServerConfig>,
    pub search_domain: Vec<Name>,
    pub domain: Option<Name>,
}

#[derive(Clone, Debug)]
pub struct ForgeResolveConf {
    parsed_configuration: Option<resolv_conf::Config>,
}

#[derive(thiserror::Error, Debug)]
pub enum ResolverError {
    #[error("could not read resolv.conf at {path}: {error}")]
    CouldNotReadResolvConf { path: PathBuf, error: io::Error },
    #[error("could not parse resolv.conf at {path}: {error}")]
    CouldNotParseResolvConf {
        path: PathBuf,
        error: resolv_conf::ParseError,
    },
    #[error("error resolving host {string}: {error}")]
    InvalidHostString {
        string: String,
        error: hickory_resolver::proto::ProtoError,
    },
}

impl ForgeResolveConf {
    pub fn new(path: &Path) -> Result<Self, ResolverError> {
        let resolv_conf_file = Path::new(&path);
        let parsed_data = read_resolv_conf(resolv_conf_file)?;

        Ok(Self {
            parsed_configuration: Some(parsed_data),
        })
    }

    pub fn with_system_resolv_conf() -> Result<Self, ResolverError> {
        let resolv_conf_file = Path::new(RESOLV_CONF_PATH);
        let parsed_data = read_resolv_conf(resolv_conf_file)?;

        Ok(Self {
            parsed_configuration: Some(parsed_data),
        })
    }

    pub fn parsed_configuration(self) -> resolv_conf::Config {
        self.parsed_configuration.unwrap_or_default()
    }
}

impl ForgeResolverConfig {
    pub fn new() -> Self {
        Self {
            inner: vec![],
            search_domain: vec![],
            domain: None,
        }
    }
}

pub fn into_forge_resolver_config(
    parsed_config: resolv_conf::Config,
) -> Result<(ForgeResolverConfig, ResolverOpts), ResolverError> {
    let mut frc = ForgeResolverConfig::new();

    if let Some(domain) = parsed_config.get_domain() {
        frc.domain = Some(Name::from_str(domain.as_str()).map_err(|error| {
            ResolverError::InvalidHostString {
                string: domain.to_string(),
                error,
            }
        })?);
    } else {
        frc.domain = None
    }

    let nameserver_configs: Vec<NameServerConfig> = parsed_config
        .get_nameservers_or_local()
        .into_iter()
        .map(|scoped_ip| NameServerConfig::udp_and_tcp(scoped_ip.into()))
        .collect();

    if nameserver_configs.is_empty() {
        tracing::warn!("no nameservers found in config");
    }

    for search_domain in parsed_config.get_last_search_or_domain() {
        // Ignore invalid search domains
        if search_domain == "--" {
            continue;
        }

        frc.search_domain
            .push(Name::from_str_relaxed(search_domain).map_err(|error| {
                ResolverError::InvalidHostString {
                    string: search_domain.to_string(),
                    error,
                }
            })?);
    }

    frc.inner = nameserver_configs;

    // TODO: Allow passing through Custom ResolverOpts
    Ok((frc, ResolverOpts::default()))
}
