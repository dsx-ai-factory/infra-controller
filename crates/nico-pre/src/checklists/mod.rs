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
//! the table. A check that is registered but not written yet is a `todo`
//! entry and shows as a ⚠ row until it lands.

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

    /// Every check carries its spec: the labelled parts in order, `Warn:` optional,
    /// so an engineer can implement a `todo` entry from the table alone.
    #[test]
    fn every_check_carries_a_complete_spec() {
        const ORDER: [&str; 6] = ["Reads:", "Pass:", "Warn:", "Fail:", "Skip:", "Test:"];
        for m in Checklist::ALL {
            for c in m.checks() {
                let labels: Vec<&str> = c
                    .detail
                    .lines()
                    .map(|l| l.split_once(' ').map_or(l, |(label, _)| label))
                    .collect();
                let mut want: Vec<&str> = ORDER.to_vec();
                if !labels.contains(&"Warn:") {
                    want.retain(|l| *l != "Warn:");
                }
                assert_eq!(labels, want, "{}: spec parts out of order or missing", c.id);
                for line in c.detail.lines() {
                    assert!(
                        line.split_once(' ')
                            .is_some_and(|(_, rest)| !rest.trim().is_empty()),
                        "{}: empty spec part `{line}`",
                        c.id
                    );
                }
            }
        }
    }
}
