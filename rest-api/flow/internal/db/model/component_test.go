// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"os"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type testComponent struct {
	c *Component
}

func newTestComponent(c Component) *testComponent {
	return &testComponent{c: &c}
}

func (tc *testComponent) Component() *Component {
	return tc.c
}

func (tc *testComponent) modifyFirmwareVersion(version string) *testComponent {
	tc.c.FirmwareVersion = version
	return tc
}

func (tc *testComponent) modifyDescription(desc map[string]any) *testComponent {
	tc.c.Description = desc
	return tc
}

func (tc *testComponent) modifyRackID(rackID uuid.UUID) *testComponent {
	tc.c.RackID = rackID
	return tc
}

func (tc *testComponent) modifySlotID(slotID int) *testComponent {
	tc.c.SlotID = slotID
	return tc
}

func (tc *testComponent) modifyTrayIndex(trayIndex int) *testComponent {
	tc.c.TrayIndex = trayIndex
	return tc
}

func (tc *testComponent) modifyHostID(hostID int) *testComponent {
	tc.c.HostID = hostID
	return tc
}

func TestComponentBuildPatch(t *testing.T) {
	componentID := uuid.New()
	rackID1 := uuid.New()
	rackID2 := uuid.New()

	shareComponent := Component{
		ID:              componentID,
		Type:            devicetypes.ComponentTypeCompute.String(),
		Manufacturer:    "NVIDIA",
		Model:           "DGX",
		SerialNumber:    "12345",
		Description:     map[string]any{"key": "value", "nested": map[string]any{"inner": "data"}}, //nolint:gosec
		FirmwareVersion: "1.0.0",
		RackID:          rackID1,
		SlotID:          1,
		TrayIndex:       2,
		HostID:          3,
	}

	testCases := map[string]struct {
		cur      *Component
		input    *Component
		expected *Component
	}{
		"nil input Component returns nil": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    nil,
			expected: nil,
		},
		"nil current Component returns nil": {
			cur:      nil,
			input:    newTestComponent(shareComponent).Component(),
			expected: nil,
		},
		"both Components nil returns nil": {
			cur:      nil,
			input:    nil,
			expected: nil,
		},
		"no changes returns nil": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).Component(),
			expected: nil,
		},
		"firmware version change": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyFirmwareVersion("2.0.0").Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyFirmwareVersion("2.0.0").Component(), //nolint:gosec
		},
		"empty firmware version ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyFirmwareVersion("").Component(), //nolint:gosec
			expected: nil,
		},
		"same firmware version ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyFirmwareVersion("1.0.0").Component(), //nolint:gosec
			expected: nil,
		},
		"description change": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyDescription(map[string]any{"key": "new_value"}).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyDescription(map[string]any{"key": "new_value"}).Component(), //nolint:gosec
		},
		"description no change": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyDescription(map[string]any{"key": "value", "nested": map[string]any{"inner": "data"}}).Component(), //nolint:gosec
			expected: nil,
		},
		"description to nil": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyDescription(nil).Component(), //nolint:gosec
			expected: nil,
		},
		"rack ID change": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyRackID(rackID2).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyRackID(rackID2).Component(), //nolint:gosec
		},
		"nil rack ID ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyRackID(uuid.Nil).Component(), //nolint:gosec
			expected: nil,
		},
		"same rack ID ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyRackID(rackID1).Component(), //nolint:gosec
			expected: nil,
		},
		"slot ID change": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifySlotID(5).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifySlotID(5).Component(), //nolint:gosec
		},
		"slot ID to zero": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifySlotID(0).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifySlotID(0).Component(), //nolint:gosec
		},
		"negative slot ID ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifySlotID(-1).Component(), //nolint:gosec
			expected: nil,
		},
		"same slot ID ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifySlotID(1).Component(), //nolint:gosec
			expected: nil,
		},
		"tray index change": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyTrayIndex(7).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyTrayIndex(7).Component(), //nolint:gosec
		},
		"tray index to zero": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyTrayIndex(0).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyTrayIndex(0).Component(), //nolint:gosec
		},
		"negative tray index ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyTrayIndex(-1).Component(), //nolint:gosec
			expected: nil,
		},
		"same tray index ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyTrayIndex(2).Component(), //nolint:gosec
			expected: nil,
		},
		"host ID change": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyHostID(10).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyHostID(10).Component(), //nolint:gosec
		},
		"host ID to zero": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyHostID(0).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyHostID(0).Component(), //nolint:gosec
		},
		"negative host ID ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyHostID(-1).Component(), //nolint:gosec
			expected: nil,
		},
		"same host ID ignored": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyHostID(3).Component(), //nolint:gosec
			expected: nil,
		},
		"multiple changes": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyFirmwareVersion("3.0.0").modifyRackID(rackID2).modifySlotID(8).modifyTrayIndex(9).modifyHostID(10).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyFirmwareVersion("3.0.0").modifyRackID(rackID2).modifySlotID(8).modifyTrayIndex(9).modifyHostID(10).Component(), //nolint:gosec
		},
		"mixed valid and invalid changes": {
			cur:      newTestComponent(shareComponent).Component(),
			input:    newTestComponent(shareComponent).modifyFirmwareVersion("").modifyRackID(uuid.Nil).modifySlotID(-1).modifyTrayIndex(5).modifyHostID(-1).Component(), //nolint:gosec
			expected: newTestComponent(shareComponent).modifyTrayIndex(5).Component(),                                                                                    //nolint:gosec
		},
		"complex description change": {
			cur: newTestComponent(shareComponent).Component(),
			input: newTestComponent(shareComponent).modifyDescription(map[string]any{
				"key":    "updated_value",
				"nested": map[string]any{"inner": "updated_data", "new_field": "added"}, //nolint:gosec
				"array":  []string{"item1", "item2"},
			}).Component(),
			expected: newTestComponent(shareComponent).modifyDescription(map[string]any{
				"key":    "updated_value",
				"nested": map[string]any{"inner": "updated_data", "new_field": "added"}, //nolint:gosec
				"array":  []string{"item1", "item2"},
			}).Component(),
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := tc.input.BuildPatch(tc.cur)
			assert.Equal(t, tc.expected, result)
		})
	}
}

func TestGetComponentsByType(t *testing.T) {
	ctx := context.Background()

	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	dbConf, err := cdb.ConfigFromEnv()
	assert.Nil(t, err)

	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	assert.Nil(t, err)

	domain := NVLDomain{Name: "test-domain"}
	err = domain.Create(ctx, pool.DB)
	assert.Nil(t, err)

	// Create a rack (required for components)
	rack := Rack{
		Name:         "test-rack",
		Manufacturer: "TestMfg",
		SerialNumber: "rack-serial-001",
		NVLDomainID:  domain.ID,
	}
	err = rack.Create(ctx, pool.DB)
	assert.Nil(t, err)

	// Create Compute components
	// Note: use ComponentTypeToString (not .String() which adds alignment padding)
	compute1 := Component{
		Name:         "compute-1",
		Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		Manufacturer: "NVIDIA",
		SerialNumber: "comp-serial-001",
		RackID:       rack.ID,
	}
	err = compute1.Create(ctx, pool.DB)
	assert.Nil(t, err)

	compute2 := Component{
		Name:         "compute-2",
		Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		Manufacturer: "NVIDIA",
		SerialNumber: "comp-serial-002",
		RackID:       rack.ID,
	}
	err = compute2.Create(ctx, pool.DB)
	assert.Nil(t, err)

	// Create PowerShelf component with a BMC
	ps1 := Component{
		Name:         "powershelf-1",
		Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypePowerShelf),
		Manufacturer: "LiteonMfg",
		SerialNumber: "ps-serial-001",
		RackID:       rack.ID,
	}
	err = ps1.Create(ctx, pool.DB)
	assert.Nil(t, err)

	err = pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		return (&BMC{
			MacAddress:  "aa:bb:cc:dd:ee:01",
			Type:        devicetypes.BMCTypeHost.String(),
			ComponentID: ps1.ID,
		}).Create(ctx, tx)
	})
	assert.Nil(t, err)

	// Test: Get Compute components
	computes, err := GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeCompute)
	assert.Nil(t, err)
	assert.Equal(t, 2, len(computes))
	for _, c := range computes {
		assert.Equal(t, devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), c.Type)
		assert.NotNil(t, c.Rack)
		assert.Equal(t, domain.ID, c.Rack.NVLDomainID)
	}

	// Test: Get PowerShelf components (should include BMCs via Relation)
	powershelves, err := GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypePowerShelf)
	assert.Nil(t, err)
	assert.Equal(t, 1, len(powershelves))
	assert.Equal(t, "powershelf-1", powershelves[0].Name)
	assert.Equal(t, 1, len(powershelves[0].BMCs))
	assert.Equal(t, "aa:bb:cc:dd:ee:01", powershelves[0].BMCs[0].MacAddress)

	// Test: Get NVSwitch components (should be empty)
	switches, err := GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
	assert.Nil(t, err)
	assert.Equal(t, 0, len(switches))
}

func TestComponentForceDelete_Idempotent(t *testing.T) {
	ctx := context.Background()

	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	dbConf, err := cdb.ConfigFromEnv()
	assert.Nil(t, err)

	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	assert.Nil(t, err)

	rack := Rack{
		Name:         "fd-rack",
		Manufacturer: "TestMfg",
		SerialNumber: "fd-rack-serial",
	}
	err = rack.Create(ctx, pool.DB)
	assert.Nil(t, err)

	comp := Component{
		Name:         "fd-comp",
		Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		Manufacturer: "NVIDIA",
		SerialNumber: "fd-comp-serial",
		RackID:       rack.ID,
	}
	err = comp.Create(ctx, pool.DB)
	assert.Nil(t, err)

	// First ForceDelete succeeds (row exists).
	err = comp.ForceDelete(ctx, pool.DB)
	assert.Nil(t, err)

	// Second ForceDelete on the same ID also succeeds (idempotent).
	err = comp.ForceDelete(ctx, pool.DB)
	assert.Nil(t, err)

	// ForceDelete on a UUID that never existed also succeeds.
	phantom := &Component{ID: uuid.New()}
	err = phantom.ForceDelete(ctx, pool.DB)
	assert.Nil(t, err)
}
