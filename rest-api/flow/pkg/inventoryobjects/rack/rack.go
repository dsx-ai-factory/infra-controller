// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rack

import (
	"fmt"
	"slices"
	"sort"
	"strings"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

// Rack represents a hardware rack with various properties and components
// installed inside. This is not thread safe since it is not really needed.
// If the caller plans to use this in a concurrent environment, they should
// wrap it in a sync.RWMutex.
//
// NOTE: The Components field does not necessarily contain ALL components
// in the rack. Callers should not assume Components is complete - it may
// contain only a subset of components (e.g., those selected for an operation).
// Always verify the context in which a Rack object is used.
type Rack struct {
	Info        deviceinfo.DeviceInfo `json:"info"`
	ExternalID  string                `json:"external_id,omitempty"`
	Loc         location.Location     `json:"loc"`
	Components  []component.Component `json:"components"`
	NVLDomainID uuid.UUID             `json:"nvl_domain_id"`

	serialToCompIndex map[deviceinfo.SerialInfo]int
	sealed            bool
}

// ValidateComponentIDs checks that every component has a unique, non-nil ID.
// An empty component list is valid.
func (r *Rack) ValidateComponentIDs() error {
	if r == nil {
		return fmt.Errorf("rack is nil")
	}

	seen := make(map[uuid.UUID]struct{}, len(r.Components))
	for i, c := range r.Components {
		id := c.Info.ID
		if id == uuid.Nil {
			return fmt.Errorf("component %d id is required", i)
		}
		if _, exists := seen[id]; exists {
			return fmt.Errorf("component %d duplicates id %s", i, id)
		}
		seen[id] = struct{}{}
	}

	return nil
}

// ComponentsOrderBySlotID is a slice of components that can be sorted by slot ID.
// It implements sort.Interface and orders components by their Position.SlotID
// in descending order (higher slot IDs first, from top to bottom in the rack).
type ComponentsOrderBySlotID []component.Component

func (s ComponentsOrderBySlotID) Len() int      { return len(s) }
func (s ComponentsOrderBySlotID) Swap(i, j int) { s[i], s[j] = s[j], s[i] }
func (s ComponentsOrderBySlotID) Less(i, j int) bool {
	// Compare by SlotID: returns true if s[i].SlotID > s[j].SlotID
	return s[i].Position.Compare(s[j].Position, true)
}

// New creates a new Rack with the given device info and location.
func New(info deviceinfo.DeviceInfo, loc location.Location) *Rack {
	r := &Rack{
		Info: info,
		Loc:  loc,
	}

	r.serialToCompIndex = make(map[deviceinfo.SerialInfo]int)

	return r
}

// AddComponent adds a single component to the Rack.
func (r *Rack) AddComponent(comp component.Component) int {
	return r.AddComponents([]component.Component{comp})
}

// AddComponents adds multiple components to the Rack.
func (r *Rack) AddComponents(comps []component.Component) int {
	if r.sealed {
		return 0
	}

	added := 0
	for _, c := range comps {
		if _, ok := r.serialToCompIndex[c.Info.GetSerialInfo()]; !ok {
			r.serialToCompIndex[c.Info.GetSerialInfo()] = len(r.Components)
			r.Components = append(r.Components, c)
			added++
		}
	}

	return added
}

// Seal seal the rack information to make it unchangable.
func (r *Rack) Seal() bool {
	if r.sealed {
		return false
	}

	// Make sure the components are sorted by in-rack position -- from top
	// to bottom.
	r.sortComponentsByPosition()

	// Make sure the rack and all its components have IDs.
	r.verifyAndCreateID()

	r.sealed = true

	return true
}

func (r *Rack) sortComponentsByPosition() {
	sort.Sort(ComponentsOrderBySlotID(r.Components))

	// Rebuild the index map since component positions changed
	for i, c := range r.Components {
		r.serialToCompIndex[c.Info.GetSerialInfo()] = i
	}

	// The components are sorted, now figure out the tray index for each
	// component type.
	trayIndexMap := make(map[devicetypes.ComponentType]int)
	prevRackSlot := make(map[devicetypes.ComponentType]int)

	for i, c := range slices.Backward(r.Components) {
		t := c.Type
		if prevRackSlot[t] > 0 && c.Position.SlotID > prevRackSlot[t] {
			// Not the first tray for a component category and the rack slot
			// is changed, increase the tray index for the component type.
			trayIndexMap[t] = trayIndexMap[t] + 1
		}

		r.Components[i].Position.TrayIndex = trayIndexMap[t]
		r.Components[i].Position.HostID = 0
		prevRackSlot[t] = c.Position.SlotID
	}
}

// PatchComponents patches the components in the rack with the provided
// components.
func (r *Rack) PatchComponents(components []component.Component) int {
	if r.sealed {
		log.Debug().Msg("Rack is sealed, cannot patch components")
		return 0
	}

	patched := 0
	for _, c := range components {
		if i, ok := r.serialToCompIndex[c.Info.GetSerialInfo()]; ok {
			if r.Components[i].Patch(c, true) {
				patched++
			}
		}
	}

	return patched
}

// IsSealed checks if the rack is sealed.
func (r *Rack) IsSealed() bool {
	return r.sealed
}

// String returns a string representation of the Rack.
func (r *Rack) String() string {
	var builder strings.Builder

	builder.WriteString(fmt.Sprintf(
		"Name: %s, Manufacturer: %s, SN: %s, Location: %s",
		r.Info.Name,
		r.Info.Manufacturer,
		r.Info.SerialNumber,
		r.Loc.Encode(),
	))

	for i, c := range r.Components {
		builder.WriteString(fmt.Sprintf("\n[%2d] %v", i, &c))
	}

	return builder.String()
}

// VerifyAndCreateID verifies the IDs of the components and creates new
// ones if they are nil.
func (r *Rack) verifyAndCreateID() {
	// We can perform this operation even if the rack is sealed.
	// Once the rack ID is created, it is not changed.
	for i, c := range r.Components {
		if c.Info.ID == uuid.Nil {
			r.Components[i].Info.ID = uuid.New()
		}
	}

	if r.Info.ID == uuid.Nil {
		r.Info.ID = uuid.New()
	}
}

// VerifyIDs verifies the existence of IDs of the components and the rack.
func (r *Rack) VerifyIDs() bool {
	for _, c := range r.Components {
		if c.Info.ID == uuid.Nil {
			return false
		}
	}

	return r.Info.ID != uuid.Nil
}

// VerifyIDOrSerial verifies the validity of ID or serial information of
// the components and the rack.
func (r *Rack) VerifyIDOrSerial() bool {
	for _, c := range r.Components {
		if !c.Info.VerifyIDOrSerial() {
			return false
		}
	}

	return r.Info.VerifyIDOrSerial()
}
