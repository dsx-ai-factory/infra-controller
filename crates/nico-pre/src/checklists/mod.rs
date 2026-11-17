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

//! The checklists, one per deployment step. Each checklist is a table of
//! checks in its own module (`host.rs`, `config.rs`, ...); the harness runs
//! the table. A check that is registered but not written yet has `run: None`
//! and shows as a ⚠ row until it lands.

mod build;
mod config;
mod fabric;
mod host;
mod ingest;
mod install;
mod platform;
mod stage;

use crate::check::Row;
use crate::harness::{self, Check, Ctx};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checklist {
    Config,
    Build,
    Stage,
    Host,
    Fabric,
    Platform,
    Install,
    Ingest,
}

impl Checklist {
    pub(crate) const ALL: [Checklist; 8] = [
        Checklist::Config,
        Checklist::Build,
        Checklist::Stage,
        Checklist::Host,
        Checklist::Fabric,
        Checklist::Platform,
        Checklist::Install,
        Checklist::Ingest,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Checklist::Config => "config",
            Checklist::Build => "build",
            Checklist::Stage => "stage",
            Checklist::Host => "host",
            Checklist::Fabric => "fabric",
            Checklist::Platform => "platform",
            Checklist::Install => "install",
            Checklist::Ingest => "ingest",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Checklist> {
        Checklist::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The checklist's check table.
    pub(crate) fn checks(self) -> &'static [Check] {
        match self {
            Checklist::Config => config::CHECKS,
            Checklist::Build => build::CHECKS,
            Checklist::Stage => stage::CHECKS,
            Checklist::Host => host::CHECKS,
            Checklist::Fabric => fabric::CHECKS,
            Checklist::Platform => platform::CHECKS,
            Checklist::Install => install::CHECKS,
            Checklist::Ingest => ingest::CHECKS,
        }
    }

    /// Runs the checklist's own checks through the harness. The gate has already passed.
    pub(crate) fn run(self, ctx: &Ctx, only: &[String], skip: &[String]) -> Vec<Row> {
        harness::run(self.checks(), ctx, only, skip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_ids_are_unique_and_carry_the_checklist_prefix() {
        let mut seen = std::collections::HashSet::new();
        for m in Checklist::ALL {
            for c in m.checks() {
                assert!(
                    c.id.starts_with(&format!("{}.", m.name())),
                    "{} lacks the checklist prefix",
                    c.id
                );
                assert!(seen.insert(c.id), "duplicate check id {}", c.id);
                assert!(!c.expected.is_empty(), "{} lacks expected", c.id);
            }
        }
    }

    /// Every check describes itself: `detail` says what it reads and what it reports, so
    /// the catalogue and a `run: None` entry mean something to a reader. Its shape is free.
    #[test]
    fn every_check_carries_a_description() {
        for m in Checklist::ALL {
            for c in m.checks() {
                assert!(!c.detail.trim().is_empty(), "{} lacks a description", c.id);
            }
        }
    }
}
