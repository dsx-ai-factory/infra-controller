// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
)

// The ten ingestion stages. Each counts up from 0 on the empty site readiness proved and is timed by one write stream: T0 is the stream's first dispatch. DPUs
// have no records of their own, so the DPU stage is timed by the host stream.
const (
	RackDiscovery       result.StageID = "rack.discovery"
	RackEndToEnd        result.StageID = "rack.endToEnd"
	HostDiscovery       result.StageID = "host.discovery"
	HostEndToEnd        result.StageID = "host.endToEnd"
	HostReadyDiagnostic result.StageID = "host.readyDiagnostic"
	DPUEndToEnd         result.StageID = "dpu.endToEnd"
	SwitchDiscovery     result.StageID = "switch.discovery"
	SwitchEndToEnd      result.StageID = "switch.endToEnd"
	PowerShelfDiscovery result.StageID = "powerShelf.discovery"
	PowerShelfEndToEnd  result.StageID = "powerShelf.endToEnd"
)

// Stage is one planned stage: the count that rises from 0 to N, the count of its units in the error stage (none for discovery) and the stream that times it.
type Stage struct {
	ID     result.StageID
	Filter probeapi.Filter
	Failed *probeapi.Filter
	N      uint64
	Stream Stream
}

func linked(r probeapi.RecordType) probeapi.Filter {
	return probeapi.Filter{Type: probeapi.UnitTypeExpectedRecord, RecordType: &r, Linked: new(true)}
}

func atStage(t probeapi.UnitType, s probeapi.Stage) probeapi.Filter {
	return probeapi.Filter{Type: t, Stage: &s}
}

// Stages is the ten stages, in result order, with N from the served inventory. A zero-N stage is not applicable.
func Stages(n inventory.TypeCounts) []Stage {
	ready := func(t probeapi.UnitType) probeapi.Filter { return atStage(t, probeapi.StageReady) }
	inError := func(t probeapi.UnitType) *probeapi.Filter { f := atStage(t, probeapi.StageError); return &f }
	return []Stage{
		{RackDiscovery, linked(probeapi.RecordTypeRack), nil, n.Rack, Rack},
		{RackEndToEnd, ready(probeapi.UnitTypeRack), inError(probeapi.UnitTypeRack), n.Rack, Rack},
		{HostDiscovery, linked(probeapi.RecordTypeHost), nil, n.Host, Host},
		{HostEndToEnd, hostGate(), inError(probeapi.UnitTypeHost), n.Host, Host},
		{HostReadyDiagnostic, ready(probeapi.UnitTypeHost), inError(probeapi.UnitTypeHost), n.Host, Host},
		{DPUEndToEnd, ready(probeapi.UnitTypeDpu), inError(probeapi.UnitTypeDpu), n.DPU, Host},
		{SwitchDiscovery, linked(probeapi.RecordTypeSwitch), nil, n.Switch, Switch},
		{SwitchEndToEnd, ready(probeapi.UnitTypeSwitch), inError(probeapi.UnitTypeSwitch), n.Switch, Switch},
		{PowerShelfDiscovery, linked(probeapi.RecordTypePowerShelf), nil, n.PowerShelf, PowerShelf},
		{PowerShelfEndToEnd, ready(probeapi.UnitTypePowerShelf), inError(probeapi.UnitTypePowerShelf), n.PowerShelf, PowerShelf},
	}
}

// hostGate is the Ready hosts a tenant can be given.
func hostGate() probeapi.Filter {
	f := atStage(probeapi.UnitTypeHost, probeapi.StageReady)
	f.Allocatable = new(true)
	return f
}

// countInvariant is a count that stays at its baseline through the run, read fresh before the first write: 0 on the empty site. The REST instance total is not
// read, as REST is out of v1's scope; the native instance count stays.
type countInvariant struct {
	Name   string
	Filter probeapi.Filter
	Base   uint64
}

func countInvariants() []countInvariant {
	mismatch := func(k probeapi.MismatchKind) probeapi.Filter {
		return probeapi.Filter{Type: probeapi.UnitTypeIdentityMismatch, Kind: &k}
	}
	return []countInvariant{
		{Name: "ambiguousLinks", Filter: mismatch(probeapi.MismatchKindAmbiguousLink)},
		{Name: "devicesWithoutRecord", Filter: mismatch(probeapi.MismatchKindNoExpectedRecord)},
		{Name: "nicoInstances", Filter: atStage(probeapi.UnitTypeInstance, probeapi.StageRegistered)},
	}
}
