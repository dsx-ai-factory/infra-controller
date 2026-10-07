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

use std::sync::Arc;

use ::rpc::forge_tls_client::ForgeClientConfig;
use carbide_host_support::agent_config::AgentConfig;
pub use command_line::{AgentCommand, Options, RunOptions};
use eyre::WrapErr;
use forge_tls::client_config::ClientCert;

mod command_line;

mod main_loop;

pub async fn start(cmdline: command_line::Options) -> eyre::Result<()> {
    let (agent, path) = match cmdline.config_path {
        // normal production case
        None => (AgentConfig::default(), "default".to_string()),
        // development overrides
        Some(config_path) => (
            AgentConfig::load_from(&config_path).wrap_err(format!(
                "error loading agent configuration from {}",
                config_path.display()
            ))?,
            config_path.display().to_string(),
        ),
    };
    tracing::info!(
        config_path = path.as_str(),
        ?agent,
        "Using agent configuration"
    );

    let forge_client_config = Arc::new(
        ForgeClientConfig::new(
            agent.forge_system.root_ca.clone(),
            Some(ClientCert {
                cert_path: agent.forge_system.client_cert.clone(),
                key_path: agent.forge_system.client_key.clone(),
            }),
        )
        .use_mgmt_vrf()?,
    );

    match cmdline.cmd {
        None => {
            tracing::error!("Missing cmd. Try `forge-dpu-otel-agent --help`");
        }

        Some(AgentCommand::Run(options)) => {
            main_loop::setup_and_run(forge_client_config, agent, *options)
                .await
                .wrap_err("main_loop error exit")?;
            tracing::info!("Agent exit");
        }
    }
    Ok(())
}
