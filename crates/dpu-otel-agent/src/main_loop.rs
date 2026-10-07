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
use std::time::{Duration, Instant};

use ::rpc::forge_tls_client::ForgeClientConfig;
use carbide_host_support::agent_config::AgentConfig;
use carbide_systemd::systemd;
use forge_certs::cert_renewal::ClientCertRenewer;
use forge_tls::client_config::ClientCert;
use humantime::format_duration as dt;
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::sleep;

use crate::command_line;

pub(super) async fn setup_and_run(
    forge_client_config: Arc<ForgeClientConfig>,
    agent_config: AgentConfig,
    options: command_line::RunOptions,
) -> eyre::Result<()> {
    systemd::notify_start().await?;
    tracing::info!(
        options = ?options,
        "Started forge-dpu-otel-agent"
    );

    let start = Instant::now();

    // Setup client certificate renewal
    let forge_api_server = agent_config.forge_system.api_server.clone();
    let client_cert_renewer =
        ClientCertRenewer::new(forge_api_server.clone(), Arc::clone(&forge_client_config));

    let main_loop = MainLoop {
        agent_config,
        client_cert_renewer,
        started_at: start,
    };

    main_loop.run().await
}

struct MainLoop {
    agent_config: AgentConfig,
    client_cert_renewer: ClientCertRenewer,
    started_at: Instant,
}

struct IterationResult {
    stop: bool,
    loop_period: Duration,
}

impl MainLoop {
    /// Runs the MainLoop in endless mode
    async fn run(mut self) -> Result<(), eyre::Report> {
        let mut term_signal = signal(SignalKind::terminate())?;

        let certs = ClientCert {
            cert_path: self.agent_config.forge_system.client_cert.clone(),
            key_path: self.agent_config.forge_system.client_key.clone(),
        };

        loop {
            let result = self.run_single_iteration(&certs).await?;
            if result.stop {
                return Ok(());
            }

            tokio::select! {
                biased;
                _ = term_signal.recv() => {
                    systemd::notify_stop().await?;
                    tracing::info!("TERM signal received, clean exit");
                    return Ok(());
                }
                _ = sleep(result.loop_period) => {}
            }
        }
    }

    /// Runs a single iteration of the main loop
    async fn run_single_iteration(
        &mut self,
        certs: &ClientCert,
    ) -> Result<IterationResult, eyre::Report> {
        let iteration_start = Instant::now();

        notify_watchdog().await;

        self.client_cert_renewer
            .renew_certificates_if_necessary(Some(certs))
            .await;

        let loop_period = Duration::from_secs(self.agent_config.period.main_loop_idle_secs);

        tracing::info!(
            iteration = %dt(iteration_start.elapsed()),
            uptime = %dt(self.started_at.elapsed()),
            "main cert renewal loop",
        );

        Ok(IterationResult {
            stop: false,
            loop_period,
        })
    }
}

async fn notify_watchdog() {
    if let Err(err) = systemd::notify_watchdog().await {
        tracing::error!(error = format!("{err:#}"), "systemd::notify_watchdog");
    }
}
