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

//! Which checks a run plans, in what order, and which of them cannot run
//! before any does.

use bmc_platform::Capability;

use super::model::{Check, CheckId, Inputs, Kind, Outcome, Sequenced};
use super::validate::{self, EndState};
use super::{apply, reads};
use crate::selection::Selection;

pub(crate) struct Planned {
    pub(crate) check: Check,
    /// Checks before this one that must not have failed for it to run.
    pub(crate) after: Vec<CheckId>,
    /// Why the check will not run, decided before any check runs.
    pub(crate) skip: Option<Outcome>,
}

/// Which checks a run plans.
pub(crate) enum Scope<'a> {
    /// Every read check, narrowed to the capabilities given, if any.
    Reads(&'a [Capability]),
    /// Exactly the checks named, each run alone.
    Checks(&'a [String]),
    /// The `--validate` sequence.
    Validate(EndState),
    /// The factory reset `--factory-reset-first` runs before `--validate`.
    FactoryReset,
}

pub(crate) fn plan(
    selection: &Selection,
    inputs: &Inputs,
    scope: Scope<'_>,
) -> Result<Vec<Planned>, String> {
    let catalog = catalog();
    let steps = match scope {
        Scope::Reads(only) => catalog
            .iter()
            .filter(|check| check.kind == Kind::Read)
            .filter(|check| only.is_empty() || only.contains(&check.id.capability))
            .map(|check| Sequenced::new(check.id))
            .collect(),
        Scope::Checks(ids) => ids
            .iter()
            .map(|id| named(&catalog, id).map(|check| Sequenced::new(check.id)))
            .collect::<Result<_, _>>()?,
        Scope::Validate(end_state) => {
            validate::sequence(selection.is_supported(Capability::Dpu), end_state)
        }
        Scope::FactoryReset => validate::factory_reset_first(),
    };
    steps
        .into_iter()
        .map(|step| {
            let check = *catalog
                .iter()
                .find(|check| check.id == step.id)
                .ok_or_else(|| format!("the plan names {}, which no check has", step.id))?;
            let skip = if selection.is_supported(check.id.capability) {
                inputs.missing(check.needs)
            } else {
                Some(Outcome::Unsupported(selection.reason(check.id.capability)))
            };
            Ok(Planned {
                check,
                after: step.after,
                skip,
            })
        })
        .collect()
}

/// The check `--check` names.
fn named<'a>(catalog: &'a [Check], id: &str) -> Result<&'a Check, String> {
    catalog
        .iter()
        .find(|check| check.id.to_string() == id)
        .ok_or_else(|| {
            let known: Vec<String> = catalog.iter().map(|check| check.id.to_string()).collect();
            format!("unknown check {id}; known checks: {}", known.join(", "))
        })
}

/// Every check, once each: the read checks, the apply checks, and the
/// `--validate` steps.
fn catalog() -> Vec<Check> {
    reads::checks()
        .into_iter()
        .chain(apply::checks())
        .chain(validate::checks())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// `--check` runs exactly the checks it names and refuses a name it
    /// does not know.
    #[test]
    fn checks_are_run_as_named() {
        let catalog = catalog();
        let chosen: Vec<String> = ["bmc_control.ntp", "power.state"]
            .iter()
            .map(|id| named(&catalog, id).expect("known check").id.to_string())
            .collect();
        assert_eq!(chosen, ["bmc_control.ntp", "power.state"]);

        let unknown = named(&catalog, "no.such_check").map(|check| check.id);
        assert!(
            unknown
                .as_ref()
                .is_err_and(|message| message.contains("unknown check no.such_check")),
            "{unknown:?}"
        );
    }

    /// Each check is defined once, and every step of every sequence names
    /// one, as do the checks it follows.
    #[test]
    fn sequences_name_checks_defined_once() {
        let catalog = catalog();
        let mut ids = HashSet::new();
        for check in &catalog {
            assert!(ids.insert(check.id), "{} is defined twice", check.id);
        }
        let sequences = [
            validate::sequence(false, EndState::Factory),
            validate::sequence(true, EndState::Factory),
            validate::factory_reset_first(),
        ];
        for step in sequences.iter().flatten() {
            for id in std::iter::once(&step.id).chain(&step.after) {
                assert!(ids.contains(id), "{id} is not a check");
            }
        }
    }
}
