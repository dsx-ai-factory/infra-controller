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

use model::redfish::{
    ActionRequest, RedfishActionId, RedfishCreateAction, RedfishListActionsFilter,
};

use crate as rpc;

impl From<rpc::forge::RedfishActionId> for RedfishActionId {
    fn from(id: rpc::forge::RedfishActionId) -> Self {
        RedfishActionId {
            request_id: id.request_id,
        }
    }
}

impl From<rpc::forge::RedfishListActionsRequest> for RedfishListActionsFilter {
    fn from(req: rpc::forge::RedfishListActionsRequest) -> Self {
        RedfishListActionsFilter {
            machine_ip: req.machine_ip,
        }
    }
}

impl From<rpc::forge::RedfishCreateActionRequest> for RedfishCreateAction {
    fn from(req: rpc::forge::RedfishCreateActionRequest) -> Self {
        RedfishCreateAction {
            target: req.target,
            action: req.action,
            parameters: req.parameters,
        }
    }
}

impl From<ActionRequest> for rpc::forge::RedfishAction {
    fn from(value: ActionRequest) -> Self {
        Self {
            request_id: value.request_id,
            requester: value.requester,
            approvers: value.approvers,
            approver_dates: value.approver_dates.into_iter().map(|d| d.into()).collect(),
            machine_ips: value.machine_ips,
            board_serials: value.board_serials,
            target: value.target,
            action: value.action,
            parameters: value.parameters,
            applied_at: value.applied_at.map(|t| t.into()),
            applier: value.applier,
            results: value
                .results
                .into_iter()
                .map(|r| rpc::forge::OptionalRedfishActionResult {
                    result: r.map(|r| rpc::forge::RedfishActionResult {
                        headers: r.headers,
                        status: r.status,
                        body: r.body,
                        completed_at: Some(r.completed_at.into()),
                    }),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, value_scenarios};

    use super::*;

    // `From<rpc::forge::RedfishActionId>` carries the request id through.
    #[test]
    fn redfish_action_id_from_rpc() {
        Check {
            scenario: "request id passes through",
            input: rpc::forge::RedfishActionId { request_id: 42 },
            expect: 42,
        }
        .check(|id| RedfishActionId::from(id).request_id);
    }

    // `From<rpc::forge::RedfishListActionsRequest>` carries the machine ip filter through.
    #[test]
    fn redfish_list_actions_filter_from_rpc() {
        Check {
            scenario: "machine ip passes through",
            input: rpc::forge::RedfishListActionsRequest {
                machine_ip: Some("10.0.0.1".to_string()),
            },
            expect: Some("10.0.0.1".to_string()),
        }
        .check(|req| RedfishListActionsFilter::from(req).machine_ip);
    }

    // `From<rpc::forge::RedfishCreateActionRequest>` maps action/target/parameters across.
    #[test]
    fn redfish_create_action_from_rpc() {
        value_scenarios!(
            run = |req| {
                let action = RedfishCreateAction::from(req);
                (action.action, action.target, action.parameters)
            };
            "action, target, and parameters map across" {
                rpc::forge::RedfishCreateActionRequest {
                    ips: vec!["10.0.0.1".to_string()],
                    action: "Reset".to_string(),
                    target: "/redfish/v1/Systems/1/Actions".to_string(),
                    parameters: r#"{"ResetType":"ForceRestart"}"#.to_string(),
                } => (
                    "Reset".to_string(),
                    "/redfish/v1/Systems/1/Actions".to_string(),
                    r#"{"ResetType":"ForceRestart"}"#.to_string(),
                ),
            }
        );
    }
}
