// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use eyre::WrapErr;
use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

/// Collect NICo PostgreSQL metrics. Flags override their corresponding environment variables.
#[derive(Parser)]
pub(super) struct Config {
    /// PostgreSQL hostname or Unix socket directory.
    #[arg(long, env = "PGHOST", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    host: String,
    /// PostgreSQL port, from 1 through 65535.
    #[arg(long, env = "PGPORT", default_value_t = 5432, value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
    /// Database to monitor.
    #[arg(long, env = "PGDATABASE", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    database: String,
    /// PostgreSQL login role.
    #[arg(long, env = "PGUSER", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    username: String,
    /// PostgreSQL password; an empty value is accepted for passwordless authentication.
    #[arg(long, env = "PGPASSWORD", hide_env_values = true)]
    password: String,
    /// TLS mode: disable, allow, prefer, require, verify-ca, or verify-full.
    #[arg(long, env = "PGSSLMODE", default_value = "require")]
    ssl_mode: PgSslMode,
    /// Optional mounted PEM CA bundle, used by verify-ca and verify-full.
    #[arg(long, env = "PGSSLROOTCERT")]
    ssl_root_cert: Option<PathBuf>,
    /// HTTP listen address for /metrics, /health, and /ready.
    #[arg(
        long,
        env = "NICO_POSTGRES_EXPORTER_LISTEN",
        default_value = "0.0.0.0:9090"
    )]
    pub(super) listen: SocketAddr,
}

impl Config {
    pub(super) fn connect_options(&self) -> eyre::Result<PgConnectOptions> {
        let options = PgConnectOptions::new_without_pgpass()
            .host(&self.host)
            .port(self.port)
            .database(&self.database)
            .username(&self.username)
            .password(&self.password)
            .ssl_mode(self.ssl_mode)
            .application_name("nico-postgres-exporter")
            .disable_statement_logging();
        match &self.ssl_root_cert {
            Some(path) => Ok(options.ssl_root_cert_from_pem(
                std::fs::read(path).wrap_err("read PostgreSQL CA bundle")?,
            )),
            None => Ok(options),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{Discriminant, discriminant};

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn validates_connection_options() {
        scenarios!(parse:
            "connection configuration" {
                ("localhost", "5432", "verify-full") => Yields(discriminant(&PgSslMode::VerifyFull)),
                ("localhost", "5432", "disable") => Yields(discriminant(&PgSslMode::Disable)),
                ("localhost", "5432", "invalid") => Fails,
                ("localhost", "0", "require") => Fails,
                ("", "5432", "require") => Fails,
            }
        );
    }

    fn parse((host, port, ssl_mode): (&str, &str, &str)) -> Result<Discriminant<PgSslMode>, ()> {
        let config = Config::try_parse_from([
            "exporter",
            "--host",
            host,
            "--port",
            port,
            "--database",
            "postgres",
            "--username",
            "monitor",
            "--password",
            "",
            "--ssl-mode",
            ssl_mode,
        ])
        .map_err(drop)?;
        Ok(discriminant(
            &config.connect_options().map_err(drop)?.get_ssl_mode(),
        ))
    }
}
