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

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use clap::Parser;
use eyre::{Context, Result, bail, eyre};
use sqlx::ConnectOptions;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgConnectOptions;

/// Coalesce the current migration chain into a snapshot migration.
///
/// The current migrations directory is used to build the snapshot and is then
/// renamed to `migrations.pre-squash.<timestamp>`. A replacement migrations
/// directory is created containing only the snapshot.
#[derive(Parser)]
#[command(name = "squash-migrations")]
pub(super) struct Args {
    /// Postgres user.
    /// Defaults to TESTDB_USER, assuming you're running
    /// this from the repo with a loaded up .envrc.
    #[arg(long, env = "TESTDB_USER", default_value = "postgres")]
    db_user: String,

    /// PostgreSQL password.
    /// Defaults to TESTDB_PASSWORD, assuming you're running
    /// this from the repo with a loaded up .envrc.
    #[arg(long, env = "TESTDB_PASSWORD", default_value = "admin")]
    db_password: String,

    /// PostgreSQL host.
    /// Defaults to TESTDB_HOST, assuming you're running
    /// this from the repo with a loaded up .envrc.
    #[arg(long, env = "TESTDB_HOST", default_value = "localhost")]
    db_host: String,

    /// Path to the migrations directory.
    /// Defaults to our <repo_root>/crates/api-db/migrations.
    #[arg(long)]
    migrations_dir: Option<PathBuf>,
}

pub(super) async fn run(args: Args) -> Result<()> {
    let migrations_dir = args
        .migrations_dir
        .as_ref()
        .cloned()
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../api-db/migrations"))
        .canonicalize()
        .wrap_err("cannot resolve migrations directory")?;

    if !migrations_dir.is_dir() {
        bail!(
            "migrations directory does not exist: {}",
            migrations_dir.display()
        );
    }

    let now = Utc::now();
    let timestamp = now.format("%Y%m%d%H%M%S").to_string();
    let today = now.format("%Y-%m-%d").to_string();
    let filename = format!("{timestamp}_squash_snapshot.sql");
    let archive_dir = archive_path(&migrations_dir, &timestamp)?;

    if archive_dir.exists() {
        bail!("archive already exists: {}", archive_dir.display());
    }

    let temp_db = format!(
        "squash_migrations_temp_{}_{}",
        std::process::id(),
        now.timestamp()
    );
    let root_opts = connect_opts(&args.db_user, &args.db_password, &args.db_host, "postgres");
    let root_pool = sqlx::PgPool::connect_with(root_opts)
        .await
        .wrap_err("cannot connect to postgres")?;

    eprintln!("Creating temporary database: {temp_db}");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE \"{temp_db}\""
    )))
    .execute(&root_pool)
    .await
    .wrap_err("cannot create temporary database")?;

    let generated = generate_snapshot(&args, &migrations_dir, &temp_db, &today).await;

    eprintln!("Cleaning up temporary database...");
    let cleanup = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS \"{temp_db}\" WITH (FORCE)"
    )))
    .execute(&root_pool)
    .await;
    root_pool.close().await;

    let content = generated?;
    if let Err(error) = cleanup {
        eprintln!("Warning: could not drop temporary database {temp_db}: {error}");
    }

    validate_snapshot(&content)?;
    install_snapshot(&migrations_dir, &archive_dir, &filename, content.as_bytes())?;

    eprintln!();
    eprintln!("Done!");
    eprintln!("Archived migrations: {}", archive_dir.display());
    eprintln!(
        "Squash migration: {}/{}",
        migrations_dir.display(),
        filename
    );
    eprintln!("Timestamp: {timestamp}");
    eprintln!(
        "Add migration epoch: archive={}, squash_version={timestamp}",
        archive_dir.display()
    );

    Ok(())
}

async fn generate_snapshot(
    args: &Args,
    migrations_dir: &Path,
    temp_db: &str,
    today: &str,
) -> Result<String> {
    eprintln!("Running migrations via sqlx...");
    let temp_opts = connect_opts(&args.db_user, &args.db_password, &args.db_host, temp_db);
    let temp_pool = sqlx::PgPool::connect_with(temp_opts)
        .await
        .wrap_err("cannot connect to temporary database")?;
    let migrator = Migrator::new(migrations_dir)
        .await
        .wrap_err("cannot load migrations")?;

    migrator
        .run(&temp_pool)
        .await
        .wrap_err("migrations failed")?;

    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&temp_pool)
        .await
        .wrap_err("cannot count applied migrations")?;
    eprintln!("Applied {applied} migrations");
    temp_pool.close().await;

    eprintln!("Dumping schema...");
    let schema_dump = run_pg_dump(
        &[
            "--schema-only",
            "--exclude-table=public._sqlx_migrations",
            "--no-owner",
            "--no-privileges",
            "--no-tablespaces",
            "--no-comments",
            "-U",
            &args.db_user,
            "-h",
            &args.db_host,
            temp_db,
        ],
        &args.db_password,
    )?;

    eprintln!("Dumping seed data...");
    let data_dump = run_pg_dump(
        &[
            "--data-only",
            "--exclude-table=public._sqlx_migrations",
            "--inserts",
            "--on-conflict-do-nothing",
            "--no-owner",
            "--no-privileges",
            "--no-tablespaces",
            "--no-comments",
            "-U",
            &args.db_user,
            "-h",
            &args.db_host,
            temp_db,
        ],
        &args.db_password,
    )?;

    Ok(format!(
        "-- Squash Snapshot Migration\n\
         --\n\
         -- This migration contains the complete database schema as of {today}.\n\
         -- It was generated by: cargo xtask squash-migrations\n\n\
         {}\n\n\
         --\n\
         -- Seed data: rows inserted by original migrations that the schema depends on.\n\
         --\n\n\
         {}\n",
        sanitize_dump(&schema_dump),
        sanitize_dump(&data_dump)
    ))
}

fn archive_path(migrations_dir: &Path, timestamp: &str) -> Result<PathBuf> {
    let parent = migrations_dir
        .parent()
        .ok_or_else(|| eyre!("migrations directory has no parent"))?;
    let name = migrations_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| eyre!("migrations directory name is not valid UTF-8"))?;

    Ok(parent.join(format!("{name}.pre-squash.{timestamp}")))
}

fn sanitize_dump(dump: &str) -> String {
    dump.lines()
        .filter(|line| {
            !line.starts_with('\\')
                && !line.starts_with("SET search_path =")
                && !line.contains("pg_catalog.set_config('search_path', '', false)")
        })
        .map(|line| {
            line.strip_prefix("SET ")
                .map_or_else(|| line.to_owned(), |setting| format!("SET LOCAL {setting}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_snapshot(content: &str) -> Result<()> {
    if content.contains("_sqlx_migrations") {
        bail!("snapshot contains SQLx migration bookkeeping");
    }
    if content.lines().any(|line| line.starts_with('\\')) {
        bail!("snapshot contains a psql client command");
    }
    Ok(())
}

fn install_snapshot(
    migrations_dir: &Path,
    archive_dir: &Path,
    filename: &str,
    content: &[u8],
) -> Result<()> {
    let parent = migrations_dir
        .parent()
        .ok_or_else(|| eyre!("migrations directory has no parent"))?;
    let temporary_dir = parent.join(format!(".migrations-squash-{}", std::process::id()));

    if temporary_dir.exists() {
        bail!(
            "temporary output directory already exists: {}",
            temporary_dir.display()
        );
    }

    std::fs::create_dir(&temporary_dir).wrap_err("cannot create temporary output directory")?;
    if let Err(error) = std::fs::write(temporary_dir.join(filename), content) {
        let _ = std::fs::remove_dir_all(&temporary_dir);
        return Err(error).wrap_err("cannot write squash migration");
    }

    if let Err(error) = std::fs::rename(migrations_dir, archive_dir) {
        let _ = std::fs::remove_dir_all(&temporary_dir);
        return Err(error).wrap_err("cannot archive migrations directory");
    }

    if let Err(error) = std::fs::rename(&temporary_dir, migrations_dir) {
        let restore = std::fs::rename(archive_dir, migrations_dir);
        let _ = std::fs::remove_dir_all(&temporary_dir);
        return match restore {
            Ok(()) => {
                Err(error).wrap_err("cannot install squash migration; original directory restored")
            }
            Err(restore_error) => Err(eyre!(
                "cannot install squash migration ({error}); also failed to restore original directory ({restore_error})"
            )),
        };
    }

    Ok(())
}

fn connect_opts(user: &str, password: &str, host: &str, database: &str) -> PgConnectOptions {
    PgConnectOptions::new()
        .host(host)
        .username(user)
        .password(password)
        .database(database)
        .log_statements(tracing::log::LevelFilter::Off)
}

fn run_pg_dump(args: &[&str], password: &str) -> Result<String> {
    let output = Command::new("pg_dump")
        .args(args)
        .env("PGPASSWORD", password)
        .output()
        .wrap_err("failed to run pg_dump; is it installed?")?;

    if !output.status.success() {
        bail!(
            "pg_dump failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout).wrap_err("pg_dump produced non-UTF8 output")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_sanitization_removes_client_commands_and_localizes_settings() {
        let dump = "\\restrict token\nSET search_path = '';\nSELECT pg_catalog.set_config('search_path', '', false);\nCREATE TABLE public.example (id bigint);\n\\unrestrict token\n";

        assert_eq!(
            sanitize_dump(dump),
            "CREATE TABLE public.example (id bigint);"
        );
    }

    #[test]
    fn archive_path_uses_source_directory_name() {
        assert_eq!(
            archive_path(
                Path::new("/repo/crates/api-db/migrations"),
                "20260708172302"
            )
            .unwrap(),
            Path::new("/repo/crates/api-db/migrations.pre-squash.20260708172302")
        );
    }

    #[test]
    fn snapshot_validation_rejects_migration_metadata_and_client_commands() {
        assert!(validate_snapshot("CREATE TABLE public.example (id bigint);").is_ok());
        assert!(validate_snapshot("CREATE TABLE _sqlx_migrations (version bigint);").is_err());
        assert!(validate_snapshot("\\restrict token").is_err());
    }
}
