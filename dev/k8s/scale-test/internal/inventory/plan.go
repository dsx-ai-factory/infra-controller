// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventory

import (
	"fmt"
	"strings"
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

// WriteKey names one planned write; its body and digest are built by the ingestion conversion (PR G).
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
// journal never holds two bodies for one key.
func NewPlan(inv *SiteInventory) (WritePlan, error) {
	plan := WritePlan{Devices: map[Stream][]DeviceWrite{}}
	keys := map[WriteKey]bool{}
	claim := func(k WriteKey) error {
		if keys[k] {
			return fmt.Errorf("write %s is planned twice", k)
		}
		keys[k] = true
		return nil
	}
	for i := range inv.Pods {
		r := &inv.Pods[i].Records
		for _, rack := range r.Racks {
			w := RackWrites{RackID: rack.RackID, Group: GroupKey(rack.RackID), Rack: RackKey(rack.RackID)}
			if err := claim(w.Group); err != nil {
				return WritePlan{}, err
			}
			if err := claim(w.Rack); err != nil {
				return WritePlan{}, err
			}
			plan.Racks = append(plan.Racks, w)
		}
		for _, d := range r.Devices {
			var stream Stream
			switch d.Kind {
			case KindMachine:
				stream = Host
			case KindSwitch:
				stream = Switch
			case KindPowerShelf:
				stream = PowerShelf
			case KindDPU:
				return WritePlan{}, fmt.Errorf("device %s is a DPU", d.BmcMac)
			default:
				// Decode rejects an unknown kind, so this is defensive; the reason names no device-supplied value.
				return WritePlan{}, fmt.Errorf("device %s has an unknown kind", d.BmcMac)
			}
			rackID := ""
			if d.RackID != nil {
				rackID = *d.RackID
			}
			key := DeviceKey(stream, d.BmcMac)
			if err := claim(key); err != nil {
				return WritePlan{}, err
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
