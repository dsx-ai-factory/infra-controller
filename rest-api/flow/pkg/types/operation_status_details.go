// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"

// RackOperationStatusDetails explains the rack phase using the same inventory snapshot.
type RackOperationStatusDetails struct {
	TrayCountsByType TrayStatusCountsByType
}

// TrayStatusCountsByType separates participating trays by component type.
// All three types are present, including those with no inventory records.
type TrayStatusCountsByType struct {
	Compute    TrayStatusCounts
	NVSwitch   TrayStatusCounts
	PowerShelf TrayStatusCounts
}

// Phase aggregates all types without treating an empty type as an unknown tray.
func (c TrayStatusCountsByType) Phase() Phase {
	var present TrayStatusCounts
	for _, counts := range []TrayStatusCounts{c.Compute, c.NVSwitch, c.PowerShelf} {
		if counts == (TrayStatusCounts{}) {
			continue
		}
		present.Add(&ComponentOperationStatus{Phase: counts.Phase()})
	}
	return present.Phase()
}

// TrayStatusCounts partitions participating inventory records by operability phase.
// Missing and unrecognized component statuses count as Unknown, not Ready.
type TrayStatusCounts struct {
	Ready        uint32
	InUse        uint32
	Initializing uint32
	Error        uint32
	Deleting     uint32
	Unknown      uint32
}

// Add records one component, including a component whose status is missing.
func (c *TrayStatusCounts) Add(status *ComponentOperationStatus) {
	phase := PhaseUnknown
	if status != nil {
		phase = normalizedPhase(status.Phase)
	}
	switch phase {
	case PhaseReady:
		c.Ready++
	case PhaseInUse:
		c.InUse++
	case PhaseInitializing:
		c.Initializing++
	case PhaseError:
		c.Error++
	case PhaseDeleting:
		c.Deleting++
	default:
		c.Unknown++
	}
}

// Phase preserves the rack priority: Unknown > Error > Initializing > Deleting
// > InUse > Ready. No participating records also means Unknown.
func (c TrayStatusCounts) Phase() Phase {
	switch {
	case c.Unknown > 0:
		return PhaseUnknown
	case c.Error > 0:
		return PhaseError
	case c.Initializing > 0:
		return PhaseInitializing
	case c.Deleting > 0:
		return PhaseDeleting
	case c.InUse > 0:
		return PhaseInUse
	case c.Ready > 0:
		return PhaseReady
	default:
		return PhaseUnknown
	}
}

// ToProto preserves absence for callers without computed details.
func (d *RackOperationStatusDetails) ToProto() *pb.RackOperationStatusDetails {
	if d == nil {
		return nil
	}
	c := d.TrayCountsByType
	return &pb.RackOperationStatusDetails{TrayCountsByType: &pb.TrayStatusCountsByType{
		Compute: c.Compute.ToProto(), NvSwitch: c.NVSwitch.ToProto(), PowerShelf: c.PowerShelf.ToProto(),
	}}
}

// ToProto preserves all counters, including an empty component type.
func (c TrayStatusCounts) ToProto() *pb.TrayStatusCounts {
	return &pb.TrayStatusCounts{
		Ready: c.Ready, InUse: c.InUse, Initializing: c.Initializing,
		Error: c.Error, Deleting: c.Deleting, Unknown: c.Unknown,
	}
}

// FromProto converts available details; the caller handles absent messages.
func (d *RackOperationStatusDetails) FromProto(p *pb.RackOperationStatusDetails) {
	c := p.GetTrayCountsByType()
	d.TrayCountsByType.Compute.FromProto(c.GetCompute())
	d.TrayCountsByType.NVSwitch.FromProto(c.GetNvSwitch())
	d.TrayCountsByType.PowerShelf.FromProto(c.GetPowerShelf())
}

// FromProto replaces all counters; an absent type has zero counts.
func (d *TrayStatusCounts) FromProto(c *pb.TrayStatusCounts) {
	*d = TrayStatusCounts{
		Ready: c.GetReady(), InUse: c.GetInUse(), Initializing: c.GetInitializing(),
		Error: c.GetError(), Deleting: c.GetDeleting(), Unknown: c.GetUnknown(),
	}
}
