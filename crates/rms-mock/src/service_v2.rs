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

use crate::envelope::BatchOutcome;
use crate::fabric::Candidate;
use crate::{RmsMock, rms_v2};

#[tonic::async_trait]
impl RackManagerV2 for RmsMock {
    /// Begin configuring the rack's scale-up fabric manager.
    ///
    /// The response carries nothing but a job id, and an empty one is fatal:
    /// the caller reports the outcome as unknown and the rack waits forever.
    /// So the job is registered before responding, and the id it returns is
    /// the one `GetJobStatus` will answer for.
    ///
    /// A request that describes no fabric is rejected before any job exists,
    /// as the proto specifies: no configuration, an empty topology type, or
    /// no switches to configure is `INVALID_ARGUMENT`. NICo always sends the
    /// topology of the rack profile and the rack's switches.
    ///
    /// A request whose switches are all unknown to the mock is accepted, since
    /// the proto rejects only a malformed request synchronously, but its job
    /// fails and names them. Completing it instead would send the caller to
    /// read back a fabric in which no primary can be elected, and it waits on
    /// that indefinitely.
    ///
    /// The call also elects the rack's primary switch. After the job completes
    /// the caller reads the fabric back and requires exactly one enabled
    /// switch; the requested primary is honoured when it is one of the rack's
    /// simulated switches, otherwise one of those is chosen deterministically.
    async fn configure_scale_up_fabric_manager(
        &self,
        request: tonic::Request<rms_v2::ConfigureScaleUpFabricManagerRequest>,
    ) -> std::result::Result<
        tonic::Response<rms_v2::ConfigureScaleUpFabricManagerResponse>,
        tonic::Status,
    > {
        let req = request.get_ref();
        let topology_type = req
            .config
            .as_ref()
            .map(|config| config.topology_type.trim())
            .ok_or_else(|| tonic::Status::invalid_argument("config is required"))?;
        if topology_type.is_empty() {
            return Err(tonic::Status::invalid_argument(
                "config.topology_type is required",
            ));
        }

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
            None => self
                .jobs
                .start_failing("", rack_id, BatchOutcome::of(&refs).message),
        };

        Ok(tonic::Response::new(
            rms_v2::ConfigureScaleUpFabricManagerResponse { job_id },
        ))
    }
}
