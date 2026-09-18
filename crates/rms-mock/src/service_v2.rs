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

//! `RackManagerV2` service implementation.
//!
//! V2 is a separate gRPC service with a single method. Note that V1 declares a
//! method of the same name taking different message types; keeping the two
//! impls in separate files makes it hard to reach for the wrong one. The V1
//! spelling stays unimplemented.

use librms::protos::rack_manager_v2::rack_manager_v2_server::RackManagerV2;

use crate::envelope::{BatchOutcome, matched_or_not};
use crate::fabric::Candidate;
use crate::{RmsMock, rms_v2};

#[tonic::async_trait]
impl RackManagerV2 for RmsMock {
    /// Begin configuring the rack's scale-up fabric manager and elect its
    /// primary switch.
    ///
    /// A request naming no switches is `INVALID_ARGUMENT`; one whose switches
    /// are all unknown is accepted, and its job fails naming them.
    async fn configure_scale_up_fabric_manager(
        &self,
        request: tonic::Request<rms_v2::ConfigureScaleUpFabricManagerRequest>,
    ) -> std::result::Result<
        tonic::Response<rms_v2::ConfigureScaleUpFabricManagerResponse>,
        tonic::Status,
    > {
        let req = request.get_ref();
        let inventory = self.inventory.nodes();
        let refs = crate::resolve::resolve_nodes(&inventory, req.nodes.as_ref());
        let Some(first) = refs.first() else {
            return Err(tonic::Status::invalid_argument(
                "nodes is required: name at least one switch to configure the fabric on",
            ));
        };

        // One job per rack, however many nodes the request names.
        let rack_id = first.rack_id;
        let candidates: Vec<Candidate<'_>> = refs.iter().filter_map(Candidate::of).collect();
        let primary =
            self.fabric
                .elect_primary(rack_id, &candidates, req.primary_switch_node_id.as_deref());
        let job_id = match primary {
            Some(primary) => self.jobs.start(primary, rack_id),
            // No switch matched: fail the job naming them rather than
            // complete it.
            None => self.jobs.start_failing(
                "",
                rack_id,
                BatchOutcome::of(&matched_or_not(&refs)).message,
            ),
        };

        Ok(tonic::Response::new(
            rms_v2::ConfigureScaleUpFabricManagerResponse { job_id },
        ))
    }
}
