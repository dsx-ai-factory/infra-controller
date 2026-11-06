// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"

// APIRackOperationStatusDetails explains the rack's aggregate operability phase.
type APIRackOperationStatusDetails struct {
	TrayCounts APITrayStatusCounts `json:"trayCounts"`
}

// APITrayStatusCounts counts participating trays, not tasks or expected hardware.
// Every count is serialized, including zero.
type APITrayStatusCounts struct {
	Ready        uint32 `json:"ready"`
	InUse        uint32 `json:"inUse"`
	Initializing uint32 `json:"initializing"`
	Error        uint32 `json:"error"`
	Deleting     uint32 `json:"deleting"`
	Unknown      uint32 `json:"unknown"`
}

// FromProto replaces the counts with the upstream snapshot.
func (d *APIRackOperationStatusDetails) FromProto(p *flowv1.RackOperationStatusDetails) {
	c := p.GetTrayCounts()
	d.TrayCounts = APITrayStatusCounts{
		Ready: c.GetReady(), InUse: c.GetInUse(), Initializing: c.GetInitializing(),
		Error: c.GetError(), Deleting: c.GetDeleting(), Unknown: c.GetUnknown(),
	}
}
