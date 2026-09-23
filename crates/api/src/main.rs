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
use tokio_util::sync::CancellationToken;

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
            // Nothing should go before the call to carbide::run that isn't already here.
            // Everything that you think might belong here, belongs in carbide::run.
            let (ready_tx, _ready_rx) = tokio::sync::oneshot::channel();
            // The server has two separate route trees on one listener: the gRPC API
            // (always served, lives in `carbide-api-core`) and the admin web UI — the
            // HTML pages under `/admin`, which live in `carbide-api-web`. Handing the
            // web pages in here is the one thing only this crate can do: `carbide-api-web`
            // and `carbide-api-core` can't reference each other without a dependency
            // cycle, and this top-level binary is the only crate that depends on both.
            //
            // We always supply the builder; whether it's actually mounted is decided
            // downstream from the `enable_admin_ui` config flag (default true) — see
            // the core runtime. (We can't read config here: it's parsed inside `carbide::run`.)
            // See the docs on `carbide::AdminUiRoutesBuilder` for the full story.
            let admin_ui_routes_builder: Option<carbide::AdminUiRoutesBuilder> =
                Some(Box::new(carbide_api_web::routes));
            carbide::run(
                debug,
                run.config_path,
                run.site_config_path,
                CredentialConfig::default(),
                false,
                admin_ui_routes_builder,
                CancellationToken::new(),
                ready_tx,
            )
            .await?;
        }
    }
    Ok(())
}
