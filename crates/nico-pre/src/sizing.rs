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

//! The sizing table: the Postgres a control plane needs for a fleet of a given size.
//! `register/sizing.yaml` is compiled in; `--sizing-table <file>` replaces it for a run.

use std::path::Path;
use std::sync::OnceLock;

use eyre::{Result, WrapErr as _};
use serde::Deserialize;

const BUILTIN: &str = include_str!("../register/sizing.yaml");
static OVERRIDE: OnceLock<SizingTable> = OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SizingTable {
    /// Where the numbers come from; `placeholder` until a measured table replaces them.
    pub(crate) source: String,
    /// Ascending by `max_hosts`; a host count's row is the first it fits.
    pub(crate) rows: Vec<SizingRow>,
}

/// The Postgres a fleet of up to `max_hosts` needs; a column left out is not judged.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SizingRow {
    pub(crate) max_hosts: u32,
    #[serde(default)]
    pub(crate) postgres_volume_gi: Option<u64>,
    pub(crate) postgres_memory_gi: u64,
    #[serde(default)]
    pub(crate) db_connections: Option<u64>,
}

impl SizingTable {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let table: SizingTable = serde_yaml::from_str(text)?;
        if table.source.trim().is_empty() {
            eyre::bail!("`source` is empty: name where the numbers come from, or `placeholder`");
        }
        if table.rows.is_empty() {
            eyre::bail!("`rows` is empty");
        }
        if let Some(w) = table
            .rows
            .windows(2)
            .find(|w| w[0].max_hosts >= w[1].max_hosts)
        {
            eyre::bail!(
                "rows must ascend by maxHosts: {} is followed by {}",
                w[0].max_hosts,
                w[1].max_hosts
            );
        }
        Ok(table)
    }

    pub(crate) fn is_placeholder(&self) -> bool {
        self.source == "placeholder"
    }

    /// The first row a fleet of `hosts` fits; `None` beyond the last row.
    pub(crate) fn row_for(&self, hosts: u32) -> Option<&SizingRow> {
        self.rows.iter().find(|r| hosts <= r.max_hosts)
    }

    pub(crate) fn last_max_hosts(&self) -> u32 {
        self.rows.last().map_or(0, |r| r.max_hosts)
    }
}

/// The table in effect: the one `--sizing-table` named, else the built-in.
pub(crate) fn table() -> &'static SizingTable {
    static BUILTIN_TABLE: OnceLock<SizingTable> = OnceLock::new();
    OVERRIDE.get().unwrap_or_else(|| {
        BUILTIN_TABLE
            .get_or_init(|| SizingTable::parse(BUILTIN).expect("register/sizing.yaml parses"))
    })
}

/// Uses the table in `path` for this run instead of the built-in one.
pub(crate) fn load_override(path: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(path).wrap_err_with(|| format!("reading {}", path.display()))?;
    let table = SizingTable::parse(&text)
        .wrap_err_with(|| format!("{} is not a sizing table", path.display()))?;
    OVERRIDE
        .set(table)
        .map_err(|_| eyre::eyre!("a sizing table is already in use"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in table parses, ascends, names its source, and judges memory only.
    #[test]
    fn builtin_table_follows_the_chart_guidance() {
        let t = SizingTable::parse(BUILTIN).unwrap();
        assert!(!t.is_placeholder());
        assert!(
            t.source.contains("helm-prereqs/values.yaml"),
            "{}",
            t.source
        );
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.row_for(18).map(|r| r.postgres_memory_gi), Some(4));
        assert_eq!(t.row_for(1500).map(|r| r.postgres_memory_gi), Some(8));
        assert_eq!(t.row_for(5000).map(|r| r.postgres_memory_gi), Some(16));
        assert!(t.row_for(5001).is_none());
        assert!(
            t.rows
                .iter()
                .all(|r| r.postgres_volume_gi.is_none() && r.db_connections.is_none())
        );
        assert_eq!(t.last_max_hosts(), 5000);
    }

    /// A table whose rows do not ascend, or that names no source, is refused naming why.
    #[test]
    fn a_malformed_table_is_refused() {
        let e = SizingTable::parse(
            "source: guide\nrows:\n  - { maxHosts: 1024, postgresMemoryGi: 8 }\n  - { maxHosts: 256, postgresMemoryGi: 4 }\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("1024 is followed by 256"), "{e}");
        let e = SizingTable::parse("source: ''\nrows: [{ maxHosts: 1, postgresMemoryGi: 1 }]\n")
            .unwrap_err();
        assert!(e.to_string().contains("`source` is empty"), "{e}");
        let e = SizingTable::parse("source: guide\nrows: []\n").unwrap_err();
        assert!(e.to_string().contains("`rows` is empty"), "{e}");
    }
}
