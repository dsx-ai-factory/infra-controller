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

// src/result_types.rs
// This module defines different types used for working with mlxconfig
// and its results (as part of mlxconfig-runner). This provides types
// for working with queries (QueriedVariable and QueryResult), sync
// operations (SyncResult), comparisons, and changes. Things like sync
// and compare will both give back a PlannedChange, and set and sync
// will both give back a VariableChange. The idea is, when possible,
// we generate a PlannedChange, then we execute (if doing a sync),
// and any time we execute something that changes (sync or set), we
// then return back a VariableChange for things that changed.

use std::time::Duration;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::protos::mlx_device::{
    ComparisonResult as ComparisonResultPb, PlannedChange as PlannedChangePb,
    QueriedDeviceInfo as QueriedDeviceInfoPb, QueriedVariable as QueriedVariablePb,
    QueryResult as QueryResultPb, SyncResult as SyncResultPb, VariableChange as VariableChangePb,
};
use serde::{Deserialize, Serialize};

use crate::variables::value::MlxConfigValue;
use crate::variables::variable::MlxConfigVariable;

// QueriedVariable is a complete representation of a queried
// variable from the device, populating all of the fields we
// get back, including proper translation of the variable
// values (next, current, and default) to their MlxConfigValue
// representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueriedVariable {
    // variable is the variable definition from registry.
    pub variable: MlxConfigVariable,
    // current_value is the current value on the device.
    pub current_value: MlxConfigValue,
    // default_value is the default device value.
    pub default_value: MlxConfigValue,
    // next_value is the next value to be applied to
    // the device, once the device is rebooted. This
    // will be different than the current_value if a
    // change has been made without a reboot yet.
    pub next_value: MlxConfigValue,
    // modified reports whether the next value is
    // different from the default value. This is
    // reported by the device.
    pub modified: bool,
    // read_only is whether variable is read only.
    // This is reported by the device.
    pub read_only: bool,
}

// QueriedVariable provides a few methods to make
// working with them easier, including some wrappers
// to get at underlying data (such as the variable name).
impl QueriedVariable {
    // name returns the variable name.
    pub fn name(&self) -> &str {
        &self.variable.name
    }

    // description returns the variable description.
    pub fn description(&self) -> &str {
        &self.variable.description
    }

    // is_pending_change returns whether there is a pending
    // change (which we know if next_value is different from
    // current_value).
    pub fn is_pending_change(&self) -> bool {
        // TODO(chet): PartialEq *should* work here for the entire
        // value, since defs should also match. If that ends up
        // being a problem, this can be .value for each of them.
        self.current_value != self.next_value
    }
}

// QueryResult contains the complete query response, with the
// info about the device we got the response from, and a list
// of every QueriedVariable result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    // device_info contains the device information
    // parsed from the JSON response.
    pub device_info: QueriedDeviceInfo,
    // variables contains all queried variables with their
    // complete state as per the device.
    pub variables: Vec<QueriedVariable>,
}

// QueriedDeviceInfo is a struct containing the info
// returned about the queried device.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueriedDeviceInfo {
    // id is the "device" field from the JSON.
    pub device_id: Option<String>,
    pub device_type: Option<String>,
    // part_number is the "name" field from the returned
    // JSON. I don't know why they just didn't call it
    // part_number, but I'm fixing it here.
    pub part_number: Option<String>,
    pub description: Option<String>,
}

impl QueriedDeviceInfo {
    // new initializes a new empty DeviceInfo instance.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_device_id<T: Into<String>>(mut self, device_id: T) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    pub fn with_device_type<T: Into<String>>(mut self, device_type: T) -> Self {
        self.device_type = Some(device_type.into());
        self
    }

    pub fn with_part_number<T: Into<String>>(mut self, part_number: T) -> Self {
        self.part_number = Some(part_number.into());
        self
    }

    pub fn with_description<T: Into<String>>(mut self, description: T) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl QueryResult {
    // variable_count returns the number of variables
    // in the query result.
    pub fn variable_count(&self) -> usize {
        self.variables.len()
    }

    // get_variable returns a queried variable
    // from the query result.
    pub fn get_variable(&self, name: &str) -> Option<&QueriedVariable> {
        self.variables.iter().find(|v| v.name() == name)
    }

    // variable_names returns all variable names
    // from the query result variable list.
    pub fn variable_names(&self) -> Vec<&str> {
        self.variables.iter().map(|v| v.name()).collect()
    }
}

// SyncResult contains everything about the results
// of a sync operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    // variables_checked is the total number of
    // variables that were checked to sync.
    pub variables_checked: usize,
    // variables_changed is the total number of
    // variables that were actually changed.
    pub variables_changed: usize,
    // changes_applied are the actual changes
    // that ended up getting applied.
    pub changes_applied: Vec<VariableChange>,
    // execution_time is the execution time.
    #[serde(skip)]
    pub execution_time: Duration,
    // query_result contains the initial query
    // result before running the sync.
    pub query_result: QueryResult,
}

impl SyncResult {
    // summary prints a summary of the sync result -- this
    // is mainly just for the CLI reference example for now.
    pub fn summary(&self) -> String {
        format!(
            "Sync complete: {}/{} variables changed in {:?}",
            self.variables_changed, self.variables_checked, self.execution_time
        )
    }
}

// ComparisonResult is the result of a comparison operation,
// showing what would change between the provided key=val
// settings and what is actually on the device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonResult {
    // variables_checked is the total number of variables
    // that were checked.
    pub variables_checked: usize,
    // variables_needing_change is the total number of
    // variables that need to change.
    pub variables_needing_change: usize,
    // planned_changes is the list of planned changes.
    pub planned_changes: Vec<PlannedChange>,
    // query_result is the full query result from the
    // initial state check of the device.
    pub query_result: QueryResult,
}

impl ComparisonResult {
    // summary prints a summary of the comparison result -- this
    // is mainly just for the CLI reference example for now.
    pub fn summary(&self) -> String {
        format!(
            "Comparison complete: {}/{} variables would change",
            self.variables_needing_change, self.variables_checked
        )
    }
}

// PlannedChange represents a planned change for a variable
// before it is applied. It stores the variable, the current
// value we observed, and the desired value we are planning
// to apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedChange {
    // variable_name is the name of the variable that
    // would change.
    pub variable_name: String,
    // current_value is the current value on the device.
    pub current_value: MlxConfigValue,
    // desired_value is the desired value to be set.
    pub desired_value: MlxConfigValue,
}

impl PlannedChange {
    // description prints a description of the planned change -- this
    // is mainly just for the CLI reference example for now.
    pub fn description(&self) -> String {
        format!(
            "{}: {} → {}",
            self.variable_name, self.current_value, self.desired_value
        )
    }
}

// VariableChange represents a change that was successfully
// applied to a variable, containing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableChange {
    // variable_name is the variable that was changed.
    pub variable_name: String,
    // old_value is the value before the change was applied.
    pub old_value: MlxConfigValue,
    // new_value is the new value we applied (and should now
    // show as the next_value if we query again).
    pub new_value: MlxConfigValue,
}

impl VariableChange {
    // description prints a description of the change -- this
    // is mainly just for the CLI reference example for now.
    pub fn description(&self) -> String {
        format!(
            "{}: {} → {}",
            self.variable_name, self.old_value, self.new_value
        )
    }
}

// QueriedDeviceInfo conversions
impl From<QueriedDeviceInfo> for QueriedDeviceInfoPb {
    fn from(info: QueriedDeviceInfo) -> Self {
        QueriedDeviceInfoPb {
            device_id: info.device_id,
            device_type: info.device_type,
            part_number: info.part_number,
            description: info.description,
        }
    }
}

impl From<QueriedDeviceInfoPb> for QueriedDeviceInfo {
    fn from(pb: QueriedDeviceInfoPb) -> Self {
        QueriedDeviceInfo {
            device_id: pb.device_id,
            device_type: pb.device_type,
            part_number: pb.part_number,
            description: pb.description,
        }
    }
}

// QueriedVariable conversions
impl TryFrom<QueriedVariable> for QueriedVariablePb {
    type Error = RpcDataConversionError;

    fn try_from(var: QueriedVariable) -> Result<Self, Self::Error> {
        Ok(QueriedVariablePb {
            variable: Some(var.variable.into()),
            current_value: Some(var.current_value.into()),
            default_value: Some(var.default_value.into()),
            next_value: Some(var.next_value.into()),
            modified: var.modified,
            read_only: var.read_only,
        })
    }
}

impl TryFrom<QueriedVariablePb> for QueriedVariable {
    type Error = RpcDataConversionError;

    fn try_from(pb: QueriedVariablePb) -> Result<Self, Self::Error> {
        Ok(QueriedVariable {
            variable: pb
                .variable
                .ok_or(RpcDataConversionError::MissingArgument("variable"))?
                .try_into()?,
            current_value: pb
                .current_value
                .ok_or(RpcDataConversionError::MissingArgument("current_value"))?
                .try_into()?,
            default_value: pb
                .default_value
                .ok_or(RpcDataConversionError::MissingArgument("default_value"))?
                .try_into()?,
            next_value: pb
                .next_value
                .ok_or(RpcDataConversionError::MissingArgument("next_value"))?
                .try_into()?,
            modified: pb.modified,
            read_only: pb.read_only,
        })
    }
}

// QueryResult conversions
impl TryFrom<QueryResult> for QueryResultPb {
    type Error = RpcDataConversionError;

    fn try_from(result: QueryResult) -> Result<Self, Self::Error> {
        let variables: Result<Vec<_>, _> =
            result.variables.into_iter().map(|v| v.try_into()).collect();

        Ok(QueryResultPb {
            device_info: Some(result.device_info.into()),
            variables: variables?,
        })
    }
}

impl TryFrom<QueryResultPb> for QueryResult {
    type Error = RpcDataConversionError;

    fn try_from(pb: QueryResultPb) -> Result<Self, Self::Error> {
        let variables: Result<Vec<_>, _> = pb.variables.into_iter().map(|v| v.try_into()).collect();

        Ok(QueryResult {
            device_info: pb
                .device_info
                .ok_or(RpcDataConversionError::MissingArgument("device_info"))?
                .into(),
            variables: variables?,
        })
    }
}

// PlannedChange conversions
impl TryFrom<PlannedChange> for PlannedChangePb {
    type Error = RpcDataConversionError;

    fn try_from(change: PlannedChange) -> Result<Self, Self::Error> {
        Ok(PlannedChangePb {
            variable_name: change.variable_name,
            current_value: Some(change.current_value.into()),
            desired_value: Some(change.desired_value.into()),
        })
    }
}

impl TryFrom<PlannedChangePb> for PlannedChange {
    type Error = RpcDataConversionError;

    fn try_from(pb: PlannedChangePb) -> Result<Self, Self::Error> {
        Ok(PlannedChange {
            variable_name: pb.variable_name,
            current_value: pb
                .current_value
                .ok_or(RpcDataConversionError::MissingArgument("current_value"))?
                .try_into()?,
            desired_value: pb
                .desired_value
                .ok_or(RpcDataConversionError::MissingArgument("desired_value"))?
                .try_into()?,
        })
    }
}

// VariableChange conversions
impl TryFrom<VariableChange> for VariableChangePb {
    type Error = RpcDataConversionError;

    fn try_from(change: VariableChange) -> Result<Self, Self::Error> {
        Ok(VariableChangePb {
            variable_name: change.variable_name,
            old_value: Some(change.old_value.into()),
            new_value: Some(change.new_value.into()),
        })
    }
}

impl TryFrom<VariableChangePb> for VariableChange {
    type Error = RpcDataConversionError;

    fn try_from(pb: VariableChangePb) -> Result<Self, Self::Error> {
        Ok(VariableChange {
            variable_name: pb.variable_name,
            old_value: pb
                .old_value
                .ok_or(RpcDataConversionError::MissingArgument("old_value"))?
                .try_into()?,
            new_value: pb
                .new_value
                .ok_or(RpcDataConversionError::MissingArgument("new_value"))?
                .try_into()?,
        })
    }
}

// ComparisonResult conversions
impl TryFrom<ComparisonResult> for ComparisonResultPb {
    type Error = RpcDataConversionError;

    fn try_from(result: ComparisonResult) -> Result<Self, Self::Error> {
        let planned_changes: Result<Vec<_>, _> = result
            .planned_changes
            .into_iter()
            .map(|c| c.try_into())
            .collect();

        Ok(ComparisonResultPb {
            variables_checked: result.variables_checked as u64,
            variables_needing_change: result.variables_needing_change as u64,
            planned_changes: planned_changes?,
            query_result: Some(result.query_result.try_into()?),
        })
    }
}

impl TryFrom<ComparisonResultPb> for ComparisonResult {
    type Error = RpcDataConversionError;

    fn try_from(pb: ComparisonResultPb) -> Result<Self, Self::Error> {
        let planned_changes: Result<Vec<_>, _> = pb
            .planned_changes
            .into_iter()
            .map(|c| c.try_into())
            .collect();

        Ok(ComparisonResult {
            variables_checked: pb.variables_checked as usize,
            variables_needing_change: pb.variables_needing_change as usize,
            planned_changes: planned_changes?,
            query_result: pb
                .query_result
                .ok_or(RpcDataConversionError::MissingArgument("query_result"))?
                .try_into()?,
        })
    }
}

// SyncResult conversions
impl TryFrom<SyncResult> for SyncResultPb {
    type Error = RpcDataConversionError;

    fn try_from(result: SyncResult) -> Result<Self, Self::Error> {
        let changes_applied: Result<Vec<_>, _> = result
            .changes_applied
            .into_iter()
            .map(|c| c.try_into())
            .collect();

        Ok(SyncResultPb {
            variables_checked: result.variables_checked as u64,
            variables_changed: result.variables_changed as u64,
            changes_applied: changes_applied?,
            // Note: execution_time is not serialized (marked with serde(skip))
            query_result: Some(result.query_result.try_into()?),
        })
    }
}

impl TryFrom<SyncResultPb> for SyncResult {
    type Error = RpcDataConversionError;

    fn try_from(pb: SyncResultPb) -> Result<Self, Self::Error> {
        let changes_applied: Result<Vec<_>, _> = pb
            .changes_applied
            .into_iter()
            .map(|c| c.try_into())
            .collect();

        Ok(SyncResult {
            variables_checked: pb.variables_checked as usize,
            variables_changed: pb.variables_changed as usize,
            changes_applied: changes_applied?,
            // execution_time defaults to zero since it's not in protobuf
            execution_time: Duration::from_secs(0),
            query_result: pb
                .query_result
                .ok_or(RpcDataConversionError::MissingArgument("query_result"))?
                .try_into()?,
        })
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, scenarios, value_scenarios};

    use super::*;
    use crate::variables::spec::MlxVariableSpec;
    use crate::variables::value::MlxValueType;

    // ----- small constructors so each table row reads as data, not setup -----

    // int_var builds an Integer-spec variable with the given name. read_only
    // and description are fixed so rows that exercise them have a known answer.
    fn int_var(name: &str) -> MlxConfigVariable {
        MlxConfigVariable {
            name: name.to_string(),
            description: format!("desc of {name}"),
            read_only: false,
            spec: MlxVariableSpec::Integer,
        }
    }

    // int_value wraps an Integer value on an Integer-spec variable, validated.
    fn int_value(name: &str, n: i64) -> MlxConfigValue {
        MlxConfigValue::new(int_var(name), MlxValueType::Integer(n)).unwrap()
    }

    // queried builds a QueriedVariable whose current/next values let us drive
    // is_pending_change: pending == (current != next).
    fn queried(name: &str, current: i64, next: i64) -> QueriedVariable {
        QueriedVariable {
            variable: int_var(name),
            current_value: int_value(name, current),
            default_value: int_value(name, 0),
            next_value: int_value(name, next),
            modified: false,
            read_only: false,
        }
    }

    // ----- QueriedVariable accessors (total) -----

    // name and description are thin wrappers over the backing variable.
    #[test]
    fn queried_variable_name_and_description() {
        value_scenarios!(
            run = |q| q.name().to_string();
            "name returns the variable name" {
                queried("ALPHA", 1, 1) => "ALPHA".to_string(),
            }

            "empty name passes through" {
                queried("", 1, 1) => "".to_string(),
            }
        );

        value_scenarios!(
            run = |q| q.description().to_string();
            "description mirrors the backing variable" {
                queried("BETA", 1, 1) => "desc of BETA".to_string(),
            }
        );
    }

    // is_pending_change is true exactly when current_value != next_value.
    #[test]
    fn queried_variable_is_pending_change() {
        value_scenarios!(
            run = |q| q.is_pending_change();
            "current == next -> no pending change" {
                queried("X", 5, 5) => false,
            }

            "current != next -> pending change" {
                queried("X", 5, 9) => true,
            }

            "current == next at zero -> no pending change" {
                queried("X", 0, 0) => false,
            }
        );
    }

    // ----- QueryResult accessors (total) -----

    fn query_result(names: &[&str]) -> QueryResult {
        QueryResult {
            device_info: QueriedDeviceInfo::new(),
            variables: names.iter().map(|n| queried(n, 1, 1)).collect(),
        }
    }

    // variable_count is just the length of the variables vec, including 0.
    #[test]
    fn query_result_variable_count() {
        value_scenarios!(
            run = |qr| qr.variable_count();
            "empty" {
                query_result(&[]) => 0usize,
            }

            "single" {
                query_result(&["a"]) => 1usize,
            }

            "several" {
                query_result(&["a", "b", "c"]) => 3usize,
            }
        );
    }

    // get_variable finds by name; returns None when absent. We project to the
    // found name (or "<none>") so the row's expectation is a value we are sure of.
    #[test]
    fn query_result_get_variable() {
        value_scenarios!(
            run = |(name, qr)| {
                qr.get_variable(name)
                    .map(|v| v.name().to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            };
            "present in the middle" {
                ("b", query_result(&["a", "b", "c"])) => "b".to_string(),
            }

            "present at the front" {
                ("a", query_result(&["a", "b", "c"])) => "a".to_string(),
            }

            "absent yields None" {
                ("missing", query_result(&["a", "b", "c"])) => "<none>".to_string(),
            }

            "absent in empty result" {
                ("a", query_result(&[])) => "<none>".to_string(),
            }
        );
    }

    // variable_names collects every variable's name, preserving order.
    #[test]
    fn query_result_variable_names() {
        value_scenarios!(
            run = |qr| {
                qr.variable_names()
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<String>>()
            };
            "empty" {
                query_result(&[]) => Vec::<String>::new(),
            }

            "preserves order" {
                query_result(&["c", "a", "b"]) => vec!["c".to_string(), "a".to_string(), "b".to_string()],
            }
        );
    }

    // ----- summary / description formatters (total) -----

    // SyncResult::summary embeds changed/checked counts and the execution time.
    // The Duration is fixed at zero (Default) so the "{:?}" tail is "0ns".
    #[test]
    fn sync_result_summary() {
        fn sync(checked: usize, changed: usize) -> SyncResult {
            SyncResult {
                variables_checked: checked,
                variables_changed: changed,
                changes_applied: vec![],
                execution_time: Duration::default(),
                query_result: query_result(&[]),
            }
        }

        value_scenarios!(
            run = |s| s.summary();
            "none changed" {
                sync(3, 0) => "Sync complete: 0/3 variables changed in 0ns".to_string(),
            }

            "all changed" {
                sync(2, 2) => "Sync complete: 2/2 variables changed in 0ns".to_string(),
            }
        );
    }

    // ComparisonResult::summary embeds needing-change/checked counts.
    #[test]
    fn comparison_result_summary() {
        fn cmp(checked: usize, needing: usize) -> ComparisonResult {
            ComparisonResult {
                variables_checked: checked,
                variables_needing_change: needing,
                planned_changes: vec![],
                query_result: query_result(&[]),
            }
        }

        value_scenarios!(
            run = |c| c.summary();
            "none needing change" {
                cmp(5, 0) => "Comparison complete: 0/5 variables would change".to_string(),
            }

            "some needing change" {
                cmp(5, 2) => "Comparison complete: 2/5 variables would change".to_string(),
            }
        );
    }

    // PlannedChange::description formats "name: current → desired". Integer
    // values render bare via to_display_string, so the arrow tail is exact.
    #[test]
    fn planned_change_description() {
        value_scenarios!(
            run = |c| c.description();
            "integer current and desired" {
                PlannedChange {
                    variable_name: "VAR".to_string(),
                    current_value: int_value("VAR", 1),
                    desired_value: int_value("VAR", 2),
                } => "VAR: 1 → 2".to_string(),
            }
        );
    }

    // VariableChange::description formats "name: old → new".
    #[test]
    fn variable_change_description() {
        value_scenarios!(
            run = |c| c.description();
            "integer old and new" {
                VariableChange {
                    variable_name: "VAR".to_string(),
                    old_value: int_value("VAR", 7),
                    new_value: int_value("VAR", 8),
                } => "VAR: 7 → 8".to_string(),
            }
        );
    }

    // ----- QueriedDeviceInfo builders (total) -----

    // new starts every field None; each with_* setter fills exactly its field.
    // We project the four fields into a tuple so one table walks every setter.
    #[test]
    fn queried_device_info_builders() {
        type Fields = (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        fn fields(i: &QueriedDeviceInfo) -> Fields {
            (
                i.device_id.clone(),
                i.device_type.clone(),
                i.part_number.clone(),
                i.description.clone(),
            )
        }

        value_scenarios!(
            run = |i| fields(&i);
            "new is all None" {
                QueriedDeviceInfo::new() => (None, None, None, None),
            }

            "with_device_id fills only device_id" {
                QueriedDeviceInfo::new().with_device_id("dev-1") => (Some("dev-1".to_string()), None, None, None),
            }

            "with_device_type fills only device_type" {
                QueriedDeviceInfo::new().with_device_type("ConnectX") => (None, Some("ConnectX".to_string()), None, None),
            }

            "with_part_number fills only part_number" {
                QueriedDeviceInfo::new().with_part_number("MCX-1") => (None, None, Some("MCX-1".to_string()), None),
            }

            "with_description fills only description" {
                QueriedDeviceInfo::new().with_description("a nic") => (None, None, None, Some("a nic".to_string())),
            }

            "all setters chain" {
                QueriedDeviceInfo::new()
                .with_device_id("dev-1")
                .with_device_type("ConnectX")
                .with_part_number("MCX-1")
                .with_description("a nic") => (
                    Some("dev-1".to_string()),
                    Some("ConnectX".to_string()),
                    Some("MCX-1".to_string()),
                    Some("a nic".to_string()),
                ),
            }
        );
    }

    // ----- QueriedDeviceInfo proto round-trips (infallible From both ways) -----

    // QueriedDeviceInfo <-> Pb is a plain field copy in both directions, so a
    // round-trip preserves every Option field, including the all-None case.
    #[test]
    fn queried_device_info_proto_round_trip() {
        type Fields = (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        fn fields(i: &QueriedDeviceInfo) -> Fields {
            (
                i.device_id.clone(),
                i.device_type.clone(),
                i.part_number.clone(),
                i.description.clone(),
            )
        }

        value_scenarios!(
            run = |info| {
                let pb: QueriedDeviceInfoPb = info.into();
                let back: QueriedDeviceInfo = pb.into();
                fields(&back)
            };
            "all None survives the round-trip" {
                QueriedDeviceInfo::new() => (None, None, None, None),
            }

            "all Some survives the round-trip" {
                QueriedDeviceInfo::new()
                .with_device_id("d")
                .with_device_type("t")
                .with_part_number("p")
                .with_description("x") => (
                    Some("d".to_string()),
                    Some("t".to_string()),
                    Some("p".to_string()),
                    Some("x".to_string()),
                ),
            }
        );
    }

    // ----- QueriedVariable proto conversions (fallible) -----

    // A complete QueriedVariable round-trips through its Pb form; we project the
    // name to confirm success. Missing each required sub-message fails, so we
    // build the Pb by hand with one field set to None per rejection row.
    #[test]
    fn queried_variable_proto_conversions() {
        // Round-trip the happy path, projecting the variable name out.
        Case {
            scenario: "complete round-trip preserves the name",
            input: queried("RT", 3, 4),
            expect: Yields("RT".to_string()),
        }
        .check(|q| -> Result<String, ()> {
            let pb: QueriedVariablePb = q.try_into().map_err(drop)?;
            let back: QueriedVariable = pb.try_into().map_err(drop)?;
            Ok(back.name().to_string())
        });

        // Each None field is a distinct MissingArgument rejection path.
        fn full_pb() -> QueriedVariablePb {
            let q = queried("M", 1, 1);
            q.try_into().expect("complete QueriedVariable converts")
        }

        scenarios!(
            run = |pb| {
                QueriedVariable::try_from(pb)
                    .map(|q| q.name().to_string())
                    .map_err(drop)
            };
            "missing variable" {
                QueriedVariablePb {
                    variable: None,
                    ..full_pb()
                } => Fails,
            }

            "missing current_value" {
                QueriedVariablePb {
                    current_value: None,
                    ..full_pb()
                } => Fails,
            }

            "missing default_value" {
                QueriedVariablePb {
                    default_value: None,
                    ..full_pb()
                } => Fails,
            }

            "missing next_value" {
                QueriedVariablePb {
                    next_value: None,
                    ..full_pb()
                } => Fails,
            }
        );
    }

    // ----- QueryResult proto conversions (fallible) -----

    // QueryResult round-trips (projecting variable_count); a missing device_info
    // fails the inbound conversion.
    #[test]
    fn query_result_proto_conversions() {
        Case {
            scenario: "round-trip preserves variable count",
            input: query_result(&["a", "b"]),
            expect: Yields(2usize),
        }
        .check(|qr| -> Result<usize, ()> {
            let pb: QueryResultPb = qr.try_into().map_err(drop)?;
            let back: QueryResult = pb.try_into().map_err(drop)?;
            Ok(back.variable_count())
        });

        Case {
            scenario: "missing device_info fails",
            input: QueryResultPb {
                device_info: None,
                variables: vec![],
            },
            expect: Fails,
        }
        .check(|pb| {
            QueryResult::try_from(pb)
                .map(|q| q.variable_count())
                .map_err(drop)
        });
    }

    // ----- PlannedChange proto conversions (fallible) -----

    #[test]
    fn planned_change_proto_conversions() {
        fn planned() -> PlannedChange {
            PlannedChange {
                variable_name: "PC".to_string(),
                current_value: int_value("PC", 1),
                desired_value: int_value("PC", 2),
            }
        }

        Case {
            scenario: "round-trip preserves the variable name",
            input: planned(),
            expect: Yields("PC".to_string()),
        }
        .check(|c| -> Result<String, ()> {
            let pb: PlannedChangePb = c.try_into().map_err(drop)?;
            let back: PlannedChange = pb.try_into().map_err(drop)?;
            Ok(back.variable_name)
        });

        fn full_pb() -> PlannedChangePb {
            planned()
                .try_into()
                .expect("complete PlannedChange converts")
        }

        scenarios!(
            run = |pb| {
                PlannedChange::try_from(pb)
                    .map(|c| c.variable_name)
                    .map_err(drop)
            };
            "missing current_value" {
                PlannedChangePb {
                    current_value: None,
                    ..full_pb()
                } => Fails,
            }

            "missing desired_value" {
                PlannedChangePb {
                    desired_value: None,
                    ..full_pb()
                } => Fails,
            }
        );
    }

    // ----- VariableChange proto conversions (fallible) -----

    #[test]
    fn variable_change_proto_conversions() {
        fn change() -> VariableChange {
            VariableChange {
                variable_name: "VC".to_string(),
                old_value: int_value("VC", 7),
                new_value: int_value("VC", 8),
            }
        }

        Case {
            scenario: "round-trip preserves the variable name",
            input: change(),
            expect: Yields("VC".to_string()),
        }
        .check(|c| -> Result<String, ()> {
            let pb: VariableChangePb = c.try_into().map_err(drop)?;
            let back: VariableChange = pb.try_into().map_err(drop)?;
            Ok(back.variable_name)
        });

        fn full_pb() -> VariableChangePb {
            change()
                .try_into()
                .expect("complete VariableChange converts")
        }

        scenarios!(
            run = |pb| {
                VariableChange::try_from(pb)
                    .map(|c| c.variable_name)
                    .map_err(drop)
            };
            "missing old_value" {
                VariableChangePb {
                    old_value: None,
                    ..full_pb()
                } => Fails,
            }

            "missing new_value" {
                VariableChangePb {
                    new_value: None,
                    ..full_pb()
                } => Fails,
            }
        );
    }

    // ----- ComparisonResult proto conversions (fallible) -----

    // ComparisonResult round-trips (projecting the checked/needing counts as a
    // tuple); a missing query_result fails the inbound conversion. The counts
    // also exercise the usize<->u64 casts in both directions.
    #[test]
    fn comparison_result_proto_conversions() {
        fn cmp() -> ComparisonResult {
            ComparisonResult {
                variables_checked: 4,
                variables_needing_change: 1,
                planned_changes: vec![PlannedChange {
                    variable_name: "PC".to_string(),
                    current_value: int_value("PC", 1),
                    desired_value: int_value("PC", 2),
                }],
                query_result: query_result(&["a"]),
            }
        }

        Case {
            scenario: "round-trip preserves the counts",
            input: cmp(),
            expect: Yields((4usize, 1usize)),
        }
        .check(|c| -> Result<(usize, usize), ()> {
            let pb: ComparisonResultPb = c.try_into().map_err(drop)?;
            let back: ComparisonResult = pb.try_into().map_err(drop)?;
            Ok((back.variables_checked, back.variables_needing_change))
        });

        Case {
            scenario: "missing query_result fails",
            input: ComparisonResultPb {
                variables_checked: 0,
                variables_needing_change: 0,
                planned_changes: vec![],
                query_result: None,
            },
            expect: Fails,
        }
        .check(|pb| {
            ComparisonResult::try_from(pb)
                .map(|c| c.variables_checked)
                .map_err(drop)
        });
    }

    // ----- SyncResult proto conversions (fallible) -----

    // SyncResult round-trips (projecting checked/changed counts). execution_time
    // is serde(skip)/not in the proto, so the inbound side always defaults it to
    // zero -- we assert that explicitly. A missing query_result fails.
    #[test]
    fn sync_result_proto_conversions() {
        fn sync() -> SyncResult {
            SyncResult {
                variables_checked: 6,
                variables_changed: 2,
                changes_applied: vec![VariableChange {
                    variable_name: "VC".to_string(),
                    old_value: int_value("VC", 7),
                    new_value: int_value("VC", 8),
                }],
                // A non-zero time that should NOT survive the proto hop.
                execution_time: Duration::from_secs(42),
                query_result: query_result(&["a"]),
            }
        }

        Case {
            scenario: "round-trip preserves counts and zeroes execution_time",
            input: sync(),
            expect: Yields((6usize, 2usize, Duration::from_secs(0))),
        }
        .check(|s| -> Result<(usize, usize, Duration), ()> {
            let pb: SyncResultPb = s.try_into().map_err(drop)?;
            let back: SyncResult = pb.try_into().map_err(drop)?;
            Ok((
                back.variables_checked,
                back.variables_changed,
                back.execution_time,
            ))
        });

        Case {
            scenario: "missing query_result fails",
            input: SyncResultPb {
                variables_checked: 0,
                variables_changed: 0,
                changes_applied: vec![],
                query_result: None,
            },
            expect: Fails,
        }
        .check(|pb| {
            SyncResult::try_from(pb)
                .map(|s| s.variables_checked)
                .map_err(drop)
        });
    }
}
