// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package component

import (
	"fmt"
	"strings"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// Component represents a hardware component with various properties and
// associated BMCs.
type Component struct {
	Type            devicetypes.ComponentType         `json:"type"`
	Info            deviceinfo.DeviceInfo             `json:"info"`
	FirmwareVersion string                            `json:"firmware_version"`
	Position        InRackPosition                    `json:"position"`
	BmcsByType      map[devicetypes.BMCType][]bmc.BMC `json:"bmcs_by_type"`
	ComponentID     string                            `json:"component_id,omitempty"`
	RackID          uuid.UUID                         `json:"rack_id"`
	RackExternalID  string                            `json:"rack_external_id,omitempty"`
	NVLDomainID     uuid.UUID                         `json:"nvl_domain_id"`
	PowerState      string                            `json:"power_state,omitempty"`
	// Status is the Flow-derived view of operability. Nil when no status
	// has been computed yet (e.g. before the first inventory sync).
	Status *types.ComponentOperationStatus `json:"status,omitempty"`
	// LeakStatus is the Flow-derived coolant leak detection status, owned by
	// the leak-detection loop. LeakStatusUnknown until the loop evaluates it.
	LeakStatus types.LeakStatus `json:"leak_status,omitempty"`

	bmcMacToID map[string]bmcID
}

type bmcID struct {
	typ   devicetypes.BMCType
	index int
}

// New creates a new Component with the given type, device info, firmware
// version, and rack position.
func New(
	t devicetypes.ComponentType,
	info *deviceinfo.DeviceInfo,
	firmwareVersion string,
	pos *InRackPosition,
) Component {
	c := Component{Type: t}
	if info != nil {
		c.Info = *info
	}

	c.FirmwareVersion = firmwareVersion

	if pos != nil {
		c.Position = *pos
	}

	c.BmcsByType = make(map[devicetypes.BMCType][]bmc.BMC)
	c.bmcMacToID = make(map[string]bmcID)

	return c
}

// AddBMC add the given BMC information to the Component.
func (c *Component) AddBMC(typ devicetypes.BMCType, info bmc.BMC) bool {
	macAddr := info.MAC.String()
	if _, ok := c.bmcMacToID[macAddr]; ok {
		return false
	}

	c.bmcMacToID[macAddr] = bmcID{typ, len(c.BmcsByType[typ])}
	c.BmcsByType[typ] = append(c.BmcsByType[typ], info)

	return true
}

// IsCompute checks if the Component is of type Compute.
func (c *Component) IsCompute() bool {
	return c.Type == devicetypes.ComponentTypeCompute
}

// Patch updates the firmware version and BMCs based on the given component.
// If allowNew is true, it adds new BMCs.
func (c *Component) Patch(p Component, allowNew bool) bool {
	patched := false
	if len(p.FirmwareVersion) > 0 &&
		strings.Compare(p.FirmwareVersion, c.FirmwareVersion) != 0 {
		c.FirmwareVersion = p.FirmwareVersion
		patched = true
	}

	for _, t := range devicetypes.BMCTypes() {
		for _, bmc := range p.BmcsByType[t] {
			if id, ok := c.bmcMacToID[bmc.MAC.String()]; ok {
				// Patch the BMC
				if c.BmcsByType[id.typ][id.index].Patch(bmc) {
					patched = true
				}
			} else {
				// Add the new BMC if allowed
				if allowNew {
					c.AddBMC(t, bmc)
				}
			}
		}
	}

	return patched
}

// String returns a string representation of the Component.
func (c *Component) String() string {
	str := fmt.Sprintf(
		"%v, %s, %28s,%28s, %16s",
		&c.Position,
		c.Type.String(),
		c.Info.Name,
		c.Info.SerialNumber,
		c.FirmwareVersion,
	)

	for _, t := range devicetypes.BMCTypes() {
		if macs, ok := c.BmcsByType[t]; ok {
			str += fmt.Sprintf(", [%v:,", t)
			for _, mac := range macs {
				str += fmt.Sprintf(" %18v", mac.MAC)
			}
			str += ("]")
		}
	}

	return str
}

// InRackPosition represents the position of a component in a rack.
type InRackPosition struct {
	SlotID    int
	TrayIndex int
	HostID    int
}

// Compare compares the current InRackPosition with another InRackPosition.
func (rc *InRackPosition) Compare(rc1 InRackPosition, upper bool) bool {
	return rc.SlotID > rc1.SlotID && upper
}

// String returns a string representation of the InRackPosition.
func (rc *InRackPosition) String() string {
	return fmt.Sprintf("%2d, %1d, %2d", rc.SlotID, rc.HostID, rc.TrayIndex)
}
