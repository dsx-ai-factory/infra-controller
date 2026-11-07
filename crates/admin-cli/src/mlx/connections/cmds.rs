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

// connections/cmds.rs
// Command handlers for connection operations.

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;

use super::args::{ConnectionsDisconnectCommand, ConnectionsShowCommand};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

// handle_show shows all active scout stream connections.
impl Run for ConnectionsShowCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let response = ctx.api_client.0.scout_stream_show_connections().await?;

        let mut connections = response.scout_stream_connections;
        connections.sort_by_key(|connection| connection.machine_id);
        match &ctx.config.format {
            OutputFormat::AsciiTable => {
                print_connections_table(&connections);
            }
            OutputFormat::Json => {
                let json = serde_json::json!({
                    "connections": connections.iter().map(|c| {
                        serde_json::json!({
                            "machine_id": c.machine_id,
                            "connect_time": c.connected_at,
                            "uptime_seconds": c.uptime_seconds,
                        })
                    }).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            OutputFormat::Yaml => {
                println!("connections:");
                for conn in connections {
                    let machine_id = match conn.machine_id.as_ref() {
                        Some(id) => id.to_string(),
                        None => "null".to_string(),
                    };
                    println!("  - machine_id: {}", machine_id);
                    println!("    connect_time: \"{}\"", conn.connected_at);
                    println!("    uptime_seconds: {}", conn.uptime_seconds);
                }
            }
            OutputFormat::Csv => {
                for conn in connections {
                    let machine_id = match conn.machine_id.as_ref() {
                        Some(id) => id.to_string(),
                        None => "null".to_string(),
                    };
                    println!(
                        "{},{},{}",
                        machine_id, conn.connected_at, conn.uptime_seconds
                    );
                }
            }
        }
        Ok(())
    }
}

// handle_disconnect disconnects an active scout stream connection.
impl Run for ConnectionsDisconnectCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: ::rpc::forge::ScoutStreamDisconnectRequest = self.into();
        let response = ctx.api_client.0.scout_stream_disconnect(request).await?;
        let machine_id = match response.machine_id.as_ref() {
            Some(id) => id.to_string(),
            None => "null".to_string(),
        };

        if response.success {
            println!("Successfully disconnected machine_id={}.", machine_id);
        } else {
            println!(
                "Failed to disconnect machine_id={} (already disconnected).",
                machine_id
            );
        }

        Ok(())
    }
}

// print_connections_table displays connections in an ASCII table format.
fn print_connections_table(connections: &[rpc::forge::ScoutStreamConnectionInfo]) {
    let mut table = Table::new();

    table.add_row(Row::new(vec![
        Cell::new("Machine ID"),
        Cell::new("Connect Time"),
        Cell::new("Uptime Seconds"),
    ]));

    for conn in connections {
        let machine_id = match conn.machine_id.as_ref() {
            Some(id) => id.to_string(),
            None => "null".to_string(),
        };
        let connect_time = if let Ok(dt) = conn.connected_at.parse::<DateTime<Utc>>() {
            Cow::Owned(dt.format("%Y-%m-%d %H:%M:%S").to_string())
        } else {
            Cow::Borrowed(&conn.connected_at)
        };

        table.add_row(Row::new(vec![
            Cell::new(&machine_id),
            Cell::new(&connect_time),
            Cell::new(&conn.uptime_seconds.to_string()),
        ]));
    }

    table.printstd();
}
