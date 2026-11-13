// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"fmt"
	"strings"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
)

// Stream is a write stream: racks first, then the three device streams in parallel (Host, Switch, PowerShelf name the device's BMC key stream).
type Stream string

const (
	Rack       Stream = "rack"
	Host       Stream = "host"
	Switch     Stream = "switch"
	PowerShelf Stream = "powerShelf"
)

// RackWrites is a rack's group write and the rack write after it; the rack is sent only after the group is accepted.
type RackWrites struct {
	RackID string
	Group  WriteKey
	Rack   WriteKey
}

// DeviceWrite is one device write and the rack it names, kept so a device whose rack is not accepted is not sent.
type DeviceWrite struct {
	RackID string
	Key    WriteKey
}

// WriteKey names one planned write; Records builds its body and digest.
type WriteKey string

// WritePlan is every write of the served inventory, in send order per stream. The bodies are filled by the conversion; this plan fixes keys, order and the rack a device
// belongs to.
type WritePlan struct {
	Racks   []RackWrites
	Devices map[Stream][]DeviceWrite
}

// GroupKey, RackKey and DeviceKey are the journal keys of a write.
func GroupKey(rackID string) WriteKey { return WriteKey("rackGroup/" + rackID) }
func RackKey(rackID string) WriteKey  { return WriteKey("rack/" + rackID) }
func DeviceKey(stream Stream, bmcMac string) WriteKey {
	return WriteKey(string(stream) + "/" + strings.ToLower(bmcMac))
}

// NewPlan plans the keyed writes for the served inventory. A DPU in the device list is an error (DPUs have no record of their own); each key is planned once, so the
// journal never holds two bodies for one key. An error names indices, never an inventory value: a key holds a MAC.
func NewPlan(inv *inventory.SiteInventory) (WritePlan, error) {
	plan := WritePlan{Devices: map[Stream][]DeviceWrite{}}
	keys, groups := map[WriteKey]bool{}, map[string]bool{}
	planned := func(k WriteKey) bool {
		if keys[k] {
			return true
		}
		keys[k] = true
		return false
	}
	for i := range inv.Pods {
		r := &inv.Pods[i].Records
		for j, rack := range r.Racks {
			w := RackWrites{RackID: rack.RackID, Group: GroupKey(rack.RackID), Rack: RackKey(rack.RackID)}
			if planned(w.Group) || planned(w.Rack) {
				return WritePlan{}, fmt.Errorf("two racks map to the same write key (pod %d, rack %d)", i, j)
			}
			// A group is written once per rack under its own ID, so two racks naming one group would send colliding bodies.
			if groups[rack.Group.RackGroupID] {
				return WritePlan{}, fmt.Errorf("two racks name the same group ID (pod %d, rack %d)", i, j)
			}
			groups[rack.Group.RackGroupID] = true
			plan.Racks = append(plan.Racks, w)
		}
		for j, d := range r.Devices {
			var stream Stream
			switch d.Kind {
			case inventory.KindMachine:
				stream = Host
			case inventory.KindSwitch:
				stream = Switch
			case inventory.KindPowerShelf:
				stream = PowerShelf
			case inventory.KindDPU:
				return WritePlan{}, fmt.Errorf("a DPU device has no expected record (pod %d, device %d)", i, j)
			default: // Decode rejects an unknown kind, so this is defensive
				return WritePlan{}, fmt.Errorf("a device has an unknown kind (pod %d, device %d)", i, j)
			}
			rackID := ""
			if d.RackID != nil {
				rackID = *d.RackID
			}
			key := DeviceKey(stream, d.BmcMac)
			if planned(key) {
				return WritePlan{}, fmt.Errorf("two devices map to the same write key (pod %d, device %d)", i, j)
			}
			plan.Devices[stream] = append(plan.Devices[stream], DeviceWrite{RackID: rackID, Key: key})
		}
	}
	return plan, nil
}

// UnitKeys is every unit key: racks and devices, not groups. Groups are prerequisites, not units.
func (p *WritePlan) UnitKeys() []WriteKey {
	keys := make([]WriteKey, 0, len(p.Racks))
	for _, r := range p.Racks {
		keys = append(keys, r.Rack)
	}
	for _, stream := range []Stream{Host, Switch, PowerShelf} {
		for _, d := range p.Devices[stream] {
			keys = append(keys, d.Key)
		}
	}
	return keys
}
