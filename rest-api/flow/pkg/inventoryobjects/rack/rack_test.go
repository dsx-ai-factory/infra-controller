// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rack

import (
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

func TestNewRack(t *testing.T) {
	info := deviceinfo.NewRandom("testing-rack", 8)
	loc := location.Location{DataCenter: "DC1", Room: "Room1"}

	rack := New(info, loc)
	assert.Equal(t, rack.Info, info)
	assert.Equal(t, rack.Loc, loc)
}

func TestRack_ValidateComponentIDs(t *testing.T) {
	firstID := uuid.New()
	tests := []struct {
		name       string
		rack       *Rack
		components []component.Component
		wantErr    string
	}{
		{name: "nil rack", wantErr: "rack is nil"},
		{name: "empty components", rack: &Rack{}},
		{
			name:       "unique component ids",
			rack:       &Rack{},
			components: []component.Component{{Info: deviceinfo.DeviceInfo{ID: firstID}}},
		},
		{
			name:       "missing component id",
			rack:       &Rack{},
			components: []component.Component{{}},
			wantErr:    "component 0 id is required",
		},
		{
			name: "duplicate component id",
			rack: &Rack{},
			components: []component.Component{
				{Info: deviceinfo.DeviceInfo{ID: firstID}},
				{Info: deviceinfo.DeviceInfo{ID: firstID}},
			},
			wantErr: "component 1 duplicates id",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.rack != nil {
				test.rack.Components = test.components
			}
			err := test.rack.ValidateComponentIDs()
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestAddComponent(t *testing.T) {
	rack := New(deviceinfo.DeviceInfo{}, location.Location{})
	comp := createTestingComponent(devicetypes.ComponentTypeCompute, 1)

	added := rack.AddComponent(comp)
	assert.Equal(t, 1, added)
	assert.Equal(t, 1, len(rack.Components))
	assert.Equal(t, comp, rack.Components[0])
}

func TestAddComponents(t *testing.T) {
	rack := New(deviceinfo.DeviceInfo{}, location.Location{})

	// Try to simulate the real GB200 rack with 3 components types and slot
	// placement. And intentionally make it a bit out of order to test the
	// sorting logic.
	testingComps := []struct {
		typ       devicetypes.ComponentType
		slotStart int
		slotEnd   int
	}{
		{devicetypes.ComponentTypeCompute, 11, 18},
		{devicetypes.ComponentTypePowerShelf, 6, 9},
		{devicetypes.ComponentTypeNVSwitch, 19, 27},
		{devicetypes.ComponentTypePowerShelf, 39, 42},
		{devicetypes.ComponentTypeCompute, 28, 37},
	}

	comps := make([]component.Component, 0)
	slotsOccupied := make(map[int]struct{})
	for _, bundle := range testingComps {
		assert.True(t, bundle.slotStart <= bundle.slotEnd)
		for slot := bundle.slotStart; slot <= bundle.slotEnd; slot++ {
			if _, ok := slotsOccupied[slot]; ok {
				t.Fatalf("Slot %d is occupied", slot)
			}
			slotsOccupied[slot] = struct{}{}
			comps = append(comps, createTestingComponent(bundle.typ, slot))
		}

	}

	added := rack.AddComponents(comps)
	assert.Equal(t, len(comps), added)
	assert.Equal(t, comps, rack.Components)
	assert.True(t, validateSerialToCompIndex(rack))

	// Add the same components again
	added = rack.AddComponents(comps)
	assert.Equal(t, 0, added)
	assert.Equal(t, comps, rack.Components)
	assert.True(t, validateSerialToCompIndex(rack))

	ok := rack.Seal()
	assert.True(t, ok)
	assert.True(t, rack.sealed)
	assert.True(t, validateSerialToCompIndex(rack))
}

func TestSeal(t *testing.T) {
	rack := New(deviceinfo.DeviceInfo{}, location.Location{})
	rack.Seal()
	assert.True(t, rack.sealed)
	//assert.True(t, rack.VerifyIDs())

	// Check that adding components after sealing does not work
	comp := createTestingComponent(devicetypes.ComponentTypeCompute, 1)
	added := rack.AddComponent(comp)
	assert.Equal(t, 0, added)
	assert.Equal(t, 0, len(rack.Components))
}

func TestPatchComponents(t *testing.T) {
	rack := New(deviceinfo.DeviceInfo{}, location.Location{})
	comp := createTestingComponent(devicetypes.ComponentTypeCompute, 1)
	comp.FirmwareVersion = "1.0.0"
	rack.AddComponent(comp)

	newComp := comp
	newComp.FirmwareVersion = "1.0.1"
	patched := rack.PatchComponents([]component.Component{newComp})
	assert.Equal(t, 1, patched)
	assert.Equal(t, 1, len(rack.Components))
	assert.Equal(t, "1.0.1", rack.Components[0].FirmwareVersion)
}

func TestVerifyAndCreateID(t *testing.T) {
	rack := New(deviceinfo.DeviceInfo{}, location.Location{})
	comp := createTestingComponent(devicetypes.ComponentTypeCompute, 1)
	comp.Info.ID = uuid.Nil
	rack.AddComponent(comp)

	rack.verifyAndCreateID()
	assert.True(t, rack.Info.ID != uuid.Nil)
	assert.True(t, len(rack.Components) == 1)
	assert.True(t, rack.Components[0].Info.ID != uuid.Nil)

	expectedRackID := rack.Info.ID
	expectedCompID := rack.Components[0].Info.ID
	assert.NoError(t, uuid.Validate(expectedRackID.String()))
	assert.NoError(t, uuid.Validate(expectedCompID.String()))

	rack.verifyAndCreateID()
	assert.Equal(t, expectedRackID, rack.Info.ID)
	assert.Equal(t, expectedCompID, rack.Components[0].Info.ID)

	expectedRackID = uuid.New()
	rack = New(deviceinfo.DeviceInfo{ID: expectedRackID}, location.Location{})
	rack.verifyAndCreateID()
	assert.Equal(t, expectedRackID, rack.Info.ID)
}

func createTestingComponent(typ devicetypes.ComponentType, slotIdx int) component.Component {
	info := deviceinfo.NewRandom(fmt.Sprintf("%s-%d", typ, slotIdx), 6)
	pos := component.InRackPosition{SlotID: slotIdx}
	return component.New(typ, &info, "1.0.0", &pos)
}

func validateSerialToCompIndex(rack *Rack) bool {
	for i, c := range rack.Components {
		if rack.serialToCompIndex[c.Info.GetSerialInfo()] != i {
			return false
		}
	}

	return true
}
