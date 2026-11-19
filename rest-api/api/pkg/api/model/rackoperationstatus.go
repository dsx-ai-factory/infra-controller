// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"

// APIRackOperationStatusDetails explains the rack's aggregate operability phase.
type APIRackOperationStatusDetails struct {
	TrayCountsByType APITrayStatusCountsByType `json:"trayCountsByType"`
}

// APITrayStatusCountsByType includes zero counts for types without inventory.
type APITrayStatusCountsByType struct {
	Compute    APITrayStatusCounts `json:"compute"`
	NVSwitch   APITrayStatusCounts `json:"nvSwitch"`
	PowerShelf APITrayStatusCounts `json:"powerShelf"`
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
	c := p.GetTrayCountsByType()
	d.TrayCountsByType.Compute.FromProto(c.GetCompute())
	d.TrayCountsByType.NVSwitch.FromProto(c.GetNvSwitch())
	d.TrayCountsByType.PowerShelf.FromProto(c.GetPowerShelf())
}

// FromProto replaces counters, retaining zero values for an absent type.
func (d *APITrayStatusCounts) FromProto(c *flowv1.TrayStatusCounts) {
	*d = APITrayStatusCounts{
		Ready: c.GetReady(), InUse: c.GetInUse(), Initializing: c.GetInitializing(),
		Error: c.GetError(), Deleting: c.GetDeleting(), Unknown: c.GetUnknown(),
	}
}
