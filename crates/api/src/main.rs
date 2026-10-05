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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

use std::path::Path;

use carbide::{Command, Options, postgres_connect_options};
use carbide_secrets::CredentialConfig;
use clap::CommandFactory;
use sqlx::PgPool;
use sqlx::postgres::PgSslMode;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let options = Options::load();
    if options.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }
    let debug = options.debug;

    let Some(sub_cmd) = options.sub_cmd else {
        return Ok(Options::command().print_long_help()?);
    };

    match sub_cmd {
        Command::Migrate(m) => {
            tracing::info!("Running migrations");
            let mut pg_connection_options = postgres_connect_options(&m.datastore)?;
            let root_cafile_path = Path::new("/var/run/secrets/spiffe.io/ca.crt");
            if root_cafile_path.exists() {
                tracing::info!("using TLS for postgres connection.");
                pg_connection_options = pg_connection_options
                    .ssl_mode(PgSslMode::Require) //TODO: move this to VerifyFull once it actually works
                    .ssl_root_cert(root_cafile_path);
            }

            let pool = PgPool::connect_with(pg_connection_options).await?;
            db::migrations::migrate(&pool).await?;
        }
        Command::Run(run) => {
            // THIS SECTION HAS BEEN INTENTIONALLY KEPT SMALL.
            // carbide::run does all the work, the only parameters it should take are things where
            // we *must* have overridden values for integration tests. Any other behavior that needs
            // to be overridden in tests should be expressed via the config files.

            // production cancel_token is driven by SIGTERM/SIGINT
            let cancel_token = carbide_utils::shutdown_handler::start()?;
            // production readiness is gated by a TCP check on the gRPC port: ready_tx is a no-op
            let (ready_tx, _ready_rx) = tokio::sync::oneshot::channel();

            carbide::run(
                debug,
                run.config_path,
                run.site_config_path,
                CredentialConfig::default(),
                false,
                cancel_token,
                ready_tx,
            )
            .await?;
        }
    }
    Ok(())
}
