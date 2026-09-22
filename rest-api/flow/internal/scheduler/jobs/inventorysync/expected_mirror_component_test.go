// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestParseLabelInt(t *testing.T) {
	// Empty input is "Core didn't write this label" — ok=true so callers
	// treat it as Core authoritatively saying zero (the unset default).
	// Non-empty unparsable input is a Core data bug — ok=false so callers
	// can preserve Flow's existing value rather than clobber it with 0.
	for _, tc := range []struct {
		in     string
		want   int
		wantOK bool
	}{
		{"", 0, true},
		{"0", 0, true},
		{"7", 7, true},
		{"-3", -3, true},
		{"abc", 0, false},   // strconv.Atoi rejects non-numeric
		{"3.14", 0, false},  // strconv.Atoi rejects floats
		{"  4  ", 0, false}, // strconv.Atoi rejects whitespace
	} {
		t.Run(tc.in, func(t *testing.T) {
			got, ok := parseLabelInt(tc.in)
			assert.Equal(t, tc.want, got)
			assert.Equal(t, tc.wantOK, ok)
		})
	}
}

func TestPopulateLabelsIntoSpec_MalformedIntMarksPreserve(t *testing.T) {
	s := expectedComponentSpec{
		Type:         "Compute",
		SerialNumber: "SN-1",
	}
	populateLabelsIntoSpec(&s, map[string]string{
		labelComponentManufacturer: "Foxconn",
		labelComponentSlotID:       "abc", // malformed
		labelComponentTrayIdx:      "1",
		labelComponentHostID:       "",
	})
	assert.True(t, s.preserveFields["slot_id"], "malformed slot_id label must mark preserve so UPDATE doesn't clobber Flow's existing value with 0")
	assert.False(t, s.preserveFields["tray_index"])
	assert.False(t, s.preserveFields["host_id"], "empty label is Core saying zero, not a malformation")
	assert.Equal(t, 0, s.SlotID, "malformed input still falls back to 0 for the spec field; the preserve flag is what gates the write")
	assert.Equal(t, 1, s.TrayIndex)
	assert.Equal(t, 0, s.HostID)
}

func TestMachineDetailToSpec(t *testing.T) {
	d := nicoapi.ExpectedMachineDetail{
		ExpectedMachineID:   "em-uuid",
		BMCMACAddress:       "aa:bb:cc:dd:ee:ff",
		BMCIPAddress:        "10.0.0.1",
		ChassisSerialNumber: "SN-001",
		RackID:              "a12",
		Name:                "node-001",
		Description:         "compute node",
		Labels: map[string]string{
			labelComponentManufacturer: "Foxconn",
			labelComponentModel:        "MGX-Compute-Gen2",
			labelComponentSlotID:       "5",
			labelComponentTrayIdx:      "1",
			labelComponentHostID:       "3",
		},
	}
	s := machineDetailToSpec(d)

	assert.Equal(t, devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), s.Type)
	assert.Equal(t, "SN-001", s.SerialNumber)
	assert.Equal(t, "Foxconn", s.Manufacturer)
	assert.Equal(t, "MGX-Compute-Gen2", s.Model)
	assert.Equal(t, "compute node", s.Description)
	assert.Equal(t, 5, s.SlotID)
	assert.Equal(t, 1, s.TrayIndex)
	assert.Equal(t, 3, s.HostID)
	assert.Equal(t, "a12", s.RackExternalID)
	assert.Equal(t, "aa:bb:cc:dd:ee:ff", s.BMC.MACAddress)
	assert.Equal(t, "10.0.0.1", s.BMC.IPAddress)
}

func TestSwitchDetailToSpec_TypeIsNVSwitch(t *testing.T) {
	s := switchDetailToSpec(nicoapi.ExpectedSwitchDetail{
		SwitchSerialNumber: "SW-1",
		BMCMACAddress:      "00:00:00:00:00:01",
		Description:        "fabric switch",
		Labels:             map[string]string{labelComponentManufacturer: "NVIDIA"},
	})
	assert.Equal(t, devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch), s.Type)
	assert.Equal(t, "SW-1", s.SerialNumber)
	assert.Equal(t, "fabric switch", s.Description)
}

func TestPowerShelfDetailToSpec_TypeIsPowerShelf(t *testing.T) {
	s := powerShelfDetailToSpec(nicoapi.ExpectedPowerShelfDetail{
		ShelfSerialNumber: "PS-1",
		BMCMACAddress:     "00:00:00:00:00:02",
		Description:       "power shelf",
		Labels:            map[string]string{labelComponentManufacturer: "NVIDIA"},
	})
	assert.Equal(t, devicetypes.ComponentTypeToString(devicetypes.ComponentTypePowerShelf), s.Type)
	assert.Equal(t, "PS-1", s.SerialNumber)
	assert.Equal(t, "power shelf", s.Description)
}

func TestSpecValid(t *testing.T) {
	base := expectedComponentSpec{
		Manufacturer: "Foxconn",
		SerialNumber: "SN-1",
		BMC:          expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:ff"},
	}
	assert.True(t, specValid(base), "complete spec should be valid")

	for name, tc := range map[string]struct {
		mutate func(*expectedComponentSpec)
		want   bool
	}{
		"missing manufacturer": {mutate: func(s *expectedComponentSpec) { s.Manufacturer = "" }, want: true},
		"missing serial":       {mutate: func(s *expectedComponentSpec) { s.SerialNumber = "" }, want: true},
		"missing both labels": {mutate: func(s *expectedComponentSpec) {
			s.Manufacturer = ""
			s.SerialNumber = ""
		}, want: true},
		"missing bmc mac": {mutate: func(s *expectedComponentSpec) { s.BMC.MACAddress = "" }, want: false},
	} {
		t.Run(name, func(t *testing.T) {
			s := base
			tc.mutate(&s)
			assert.Equal(t, tc.want, specValid(s))
		})
	}
}

func TestHostBMCMACs(t *testing.T) {
	hostType := devicetypes.BMCTypeToString(devicetypes.BMCTypeHost)

	t.Run("returns the normalised MAC of every host BMC", func(t *testing.T) {
		c := &model.Component{BMCs: []model.BMC{
			{MacAddress: "AA:BB:CC:DD:EE:FF", Type: hostType},
			{MacAddress: "aa:bb:cc:dd:ee:01", Type: hostType},
		}}
		assert.Equal(t, []string{"aa:bb:cc:dd:ee:ff", "aa:bb:cc:dd:ee:01"}, hostBMCMACs(c))
	})

	t.Run("non-host BMCs are ignored", func(t *testing.T) {
		c := &model.Component{BMCs: []model.BMC{
			{MacAddress: "aa:bb:cc:dd:ee:ff", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU)},
		}}
		assert.Empty(t, hostBMCMACs(c))
	})

	t.Run("a component with no BMC at all yields nothing", func(t *testing.T) {
		assert.Empty(t, hostBMCMACs(&model.Component{}))
	})
}

func TestStillReportedByCore(t *testing.T) {
	seenMACs := map[string]struct{}{"aa:bb:cc:dd:ee:ff": {}}
	seenNaturalKeys := map[string]struct{}{naturalKey("Foxconn", "SN-1"): {}}

	tests := []struct {
		name       string
		macs       []string
		naturalKey string
		want       bool
	}{
		{name: "MAC still reported", macs: []string{"aa:bb:cc:dd:ee:ff"}, want: true},
		{
			name:       "BMC board swapped, chassis pair still reported",
			macs:       []string{"aa:bb:cc:dd:ee:02"},
			naturalKey: naturalKey("Foxconn", "SN-1"),
			want:       true,
		},
		{
			name:       "chassis relabelled, MAC still reported",
			macs:       []string{"aa:bb:cc:dd:ee:ff"},
			naturalKey: naturalKey("Wistron", "SN-9"),
			want:       true,
		},
		{name: "neither is reported", macs: []string{"aa:bb:cc:dd:ee:02"}, naturalKey: naturalKey("Wistron", "SN-9")},
		{name: "no MAC and no pair", want: false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, stillReportedByCore(tc.macs, tc.naturalKey, seenMACs, seenNaturalKeys))
		})
	}
}

func TestComponentChassisSlotOwnedByOther(t *testing.T) {
	owner := &model.Component{ID: uuid.New(), Manufacturer: "Foxconn", SerialNumber: "SN-1"}
	flowByNaturalKey := map[string]*model.Component{
		naturalKey("Foxconn", "SN-1"): owner,
	}

	tests := []struct {
		name    string
		desired model.Component
		want    bool
	}{
		{name: "pair held by another component", desired: model.Component{ID: uuid.New(), Manufacturer: "Foxconn", SerialNumber: "SN-1"}, want: true},
		{name: "pair held by the desired component", desired: model.Component{ID: owner.ID, Manufacturer: "Foxconn", SerialNumber: "SN-1"}},
		{name: "unclaimed pair", desired: model.Component{ID: uuid.New(), Manufacturer: "Wistron", SerialNumber: "SN-9"}},
		{name: "half-populated pair", desired: model.Component{ID: uuid.New(), SerialNumber: "SN-1"}},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, componentChassisSlotOwnedByOther(flowByNaturalKey, &tc.desired))
		})
	}
}

func TestResolveRackID(t *testing.T) {
	rackUUID := uuid.New()
	rackMap := map[string]uuid.UUID{"a12": rackUUID}

	t.Run("empty external id resolves to uuid.Nil with ok=true (component intentionally unassigned)", func(t *testing.T) {
		id, ok := resolveRackID(expectedComponentSpec{RackExternalID: ""}, rackMap)
		assert.True(t, ok)
		assert.Equal(t, uuid.Nil, id)
	})
	t.Run("known external id resolves to the flow rack uuid", func(t *testing.T) {
		id, ok := resolveRackID(expectedComponentSpec{RackExternalID: "a12"}, rackMap)
		assert.True(t, ok)
		assert.Equal(t, rackUUID, id)
	})
	t.Run("unknown external id is rejected so we don't insert with a stale FK", func(t *testing.T) {
		_, ok := resolveRackID(expectedComponentSpec{RackExternalID: "ghost"}, rackMap)
		assert.False(t, ok)
	})
}

func TestComponentFromSpec(t *testing.T) {
	rackID := uuid.New()
	s := expectedComponentSpec{
		Type:         "Compute",
		Manufacturer: "Foxconn",
		SerialNumber: "SN-1",
		Model:        "MGX",
		Name:         "node-1",
		Description:  "compute node",
		SlotID:       5,
		TrayIndex:    1,
		HostID:       3,
	}
	c := componentFromSpec(s, rackID)
	assert.Equal(t, "node-1", c.Name)
	assert.Equal(t, "Compute", c.Type)
	assert.Equal(t, "Foxconn", c.Manufacturer)
	assert.Equal(t, "SN-1", c.SerialNumber)
	assert.Equal(t, "MGX", c.Model)
	assert.Equal(t, map[string]any{expectedDescriptionKey: "compute node"}, c.Description)
	assert.Empty(t, c.FirmwareVersion, "firmware_version is owned by runtime sync, mirror must leave it unset")
	assert.Equal(t, 5, c.SlotID)
	assert.Equal(t, 1, c.TrayIndex)
	assert.Equal(t, 3, c.HostID)
	assert.Equal(t, rackID, c.RackID)
}

func TestComponentFromSpec_NameFallsBackToSerial(t *testing.T) {
	s := expectedComponentSpec{Type: "Compute", Manufacturer: "Foxconn", SerialNumber: "SN-1"}
	c := componentFromSpec(s, uuid.Nil)
	assert.Equal(t, "SN-1", c.Name, "empty Core name should fall back to serial so notnull-checks downstream don't trip")
}

func TestDiffComponentFields(t *testing.T) {
	rackA := uuid.New()
	rackB := uuid.New()
	base := func() *model.Component {
		return &model.Component{
			Name:         "n",
			Manufacturer: "maker",
			SerialNumber: "serial",
			Model:        "m",
			SlotID:       1,
			TrayIndex:    2,
			HostID:       3,
			RackID:       rackA,
		}
	}

	t.Run("identical fields produce no diffs", func(t *testing.T) {
		assert.Empty(t, diffComponentFields(base(), base(), expectedComponentSpec{}))
	})

	t.Run("firmware_version drift is ignored (runtime-owned, not mirrored)", func(t *testing.T) {
		desired := base()
		desired.FirmwareVersion = "2.0"
		assert.Empty(t, diffComponentFields(base(), desired, expectedComponentSpec{}))
	})

	for _, tc := range []struct {
		name        string
		existing    map[string]any
		expected    string
		wantChanged bool
	}{
		{
			name:        "new expected description is detected",
			expected:    "new description",
			wantChanged: true,
		},
		{
			name:     "unchanged expected description produces no diff",
			existing: map[string]any{expectedDescriptionKey: "same", "operator": "keep"},
			expected: "same",
		},
		{
			name:        "cleared expected description is detected",
			existing:    map[string]any{expectedDescriptionKey: "old", "operator": "keep"},
			wantChanged: true,
		},
		{
			name:     "unrelated description entries do not produce a diff",
			existing: map[string]any{"operator": "keep"},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			existing := base()
			existing.Description = tc.existing
			diffs := diffComponentFields(existing, base(), expectedComponentSpec{Description: tc.expected})
			if tc.wantChanged {
				require.Len(t, diffs, 1)
				assert.Equal(t, expectedDescriptionKey, diffs[0].field)
			} else {
				assert.Empty(t, diffs)
			}
		})
	}

	for name, mutate := range map[string]func(*model.Component){
		"name":          func(c *model.Component) { c.Name = "n2" },
		"manufacturer":  func(c *model.Component) { c.Manufacturer = "new-maker" },
		"serial_number": func(c *model.Component) { c.SerialNumber = "new-serial" },
		"model":         func(c *model.Component) { c.Model = "m2" },
		"slot_id":       func(c *model.Component) { c.SlotID = 9 },
		"tray_index":    func(c *model.Component) { c.TrayIndex = 9 },
		"host_id":       func(c *model.Component) { c.HostID = 9 },
		"rack_id":       func(c *model.Component) { c.RackID = rackB },
	} {
		t.Run("change in "+name+" is detected", func(t *testing.T) {
			desired := base()
			mutate(desired)
			diffs := diffComponentFields(base(), desired, expectedComponentSpec{})
			require.Len(t, diffs, 1)
			assert.Equal(t, name, diffs[0].field)
		})
	}

	t.Run("preserved fields don't surface as drift even when desired differs", func(t *testing.T) {
		desired := base()
		desired.SlotID = 9
		desired.TrayIndex = 9
		desired.HostID = 9
		spec := expectedComponentSpec{
			preserveFields: map[string]bool{"slot_id": true, "tray_index": true, "host_id": true},
		}
		assert.Empty(t, diffComponentFields(base(), desired, spec),
			"preserve flags suppress diffs so a malformed Core label can't drive a spurious UPDATE")
	})
}

func TestApplyComponentChanges_ConvergesCoreFieldsWithoutTouchingIdentityOrRuntimeFields(t *testing.T) {
	id := uuid.New()
	rackA := uuid.New()
	rackB := uuid.New()
	extID := "runtime-id"
	existing := &model.Component{
		ID:           id,
		Name:         "old",
		Type:         "Compute",
		Manufacturer: "Foxconn",
		SerialNumber: "SN-1",
		Model:        "old-model",
		RackID:       rackA,
		ComponentID:  &extID, // runtime-owned, must not be touched
		Description: map[string]any{
			"nvos_ip":  "10.0.0.2",
			"operator": "keep",
		},
	}
	desired := &model.Component{
		Name:         "new",
		Manufacturer: "Wistron",
		SerialNumber: "SN-2",
		Model:        "new-model",
		RackID:       rackB,
	}

	applyComponentChanges(existing, desired, expectedComponentSpec{Description: "Core description"})

	assert.Equal(t, "new", existing.Name)
	assert.Equal(t, "new-model", existing.Model)
	assert.Equal(t, rackB, existing.RackID)
	assert.Equal(t, "Compute", existing.Type, "Type is identity; mirror must not touch")
	assert.Equal(t, "Wistron", existing.Manufacturer, "manufacturer is Core-owned metadata")
	assert.Equal(t, "SN-2", existing.SerialNumber, "serial_number is Core-owned metadata")
	require.NotNil(t, existing.ComponentID)
	assert.Equal(t, "runtime-id", *existing.ComponentID, "external_id is runtime-owned")
	assert.Equal(t, "10.0.0.2", existing.Description["nvos_ip"], "runtime-owned description entry must survive")
	assert.Equal(t, "keep", existing.Description["operator"], "operator-owned description entry must survive")
	assert.Equal(t, "Core description", existing.Description[expectedDescriptionKey])
}

func TestComponentDescriptionWithExpected(t *testing.T) {
	existing := map[string]any{
		expectedDescriptionKey: "old",
		"operator":             "keep",
	}

	updated := componentDescriptionWithExpected(existing, "new")
	assert.Equal(t, map[string]any{expectedDescriptionKey: "new", "operator": "keep"}, updated)
	assert.Equal(t, "old", existing[expectedDescriptionKey], "helper must not mutate the input map")

	cleared := componentDescriptionWithExpected(updated, "")
	assert.Equal(t, map[string]any{"operator": "keep"}, cleared)
	assert.Nil(t, componentDescriptionWithExpected(map[string]any{expectedDescriptionKey: "old"}, ""))
}

func TestApplyComponentChanges_PreservedFieldsKeepFlowValue(t *testing.T) {
	existing := &model.Component{
		Name:      "n",
		Model:     "m",
		SlotID:    7, // Flow's existing value — must survive
		TrayIndex: 8,
		HostID:    9,
	}
	desired := &model.Component{
		Name:      "n",
		Model:     "m",
		SlotID:    0, // would-be overwrite from parseLabelInt fallback
		TrayIndex: 0,
		HostID:    0,
	}
	spec := expectedComponentSpec{
		preserveFields: map[string]bool{"slot_id": true, "tray_index": true, "host_id": true},
	}
	applyComponentChanges(existing, desired, spec)
	assert.Equal(t, 7, existing.SlotID, "preserve flag must protect Flow's value from malformed-label fallback zero")
	assert.Equal(t, 8, existing.TrayIndex)
	assert.Equal(t, 9, existing.HostID)
}

func TestPlanBMCReconciliation(t *testing.T) {
	compID := uuid.New()

	t.Run("no existing bmc -> insert", func(t *testing.T) {
		c := &model.Component{ID: compID, BMCs: nil}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:01", IPAddress: "10.0.0.1"})
		require.NotNil(t, ops.insert)
		assert.Nil(t, ops.update)
		assert.Empty(t, ops.deletes)
		assert.Equal(t, "aa:bb:cc:dd:ee:01", ops.insert.MacAddress)
		assert.Equal(t, compID, ops.insert.ComponentID)
		require.NotNil(t, ops.insert.IPAddress)
		assert.Equal(t, "10.0.0.1", *ops.insert.IPAddress)
		assert.Equal(t, devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), ops.insert.Type)
	})

	t.Run("same mac, same ip -> no op", func(t *testing.T) {
		ip := "10.0.0.1"
		c := &model.Component{
			ID:   compID,
			BMCs: []model.BMC{{MacAddress: "aa:bb:cc:dd:ee:01", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), IPAddress: &ip, ComponentID: compID}},
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:01", IPAddress: "10.0.0.1"})
		assert.Nil(t, ops.insert)
		assert.Nil(t, ops.update)
		assert.Empty(t, ops.deletes)
	})

	t.Run("same mac, different ip -> update only", func(t *testing.T) {
		ip := "10.0.0.1"
		c := &model.Component{
			ID:   compID,
			BMCs: []model.BMC{{MacAddress: "aa:bb:cc:dd:ee:01", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), IPAddress: &ip, ComponentID: compID}},
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:01", IPAddress: "10.0.0.2"})
		require.NotNil(t, ops.update)
		assert.Nil(t, ops.insert)
		assert.Empty(t, ops.deletes)
		require.NotNil(t, ops.update.IPAddress)
		assert.Equal(t, "10.0.0.2", *ops.update.IPAddress)
	})

	t.Run("different mac -> delete old + insert new", func(t *testing.T) {
		ip := "10.0.0.1"
		c := &model.Component{
			ID:   compID,
			BMCs: []model.BMC{{MacAddress: "aa:bb:cc:dd:ee:01", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), IPAddress: &ip, ComponentID: compID}},
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:99", IPAddress: "10.0.0.9"})
		require.Len(t, ops.deletes, 1)
		require.NotNil(t, ops.insert)
		assert.Nil(t, ops.update)
		assert.Equal(t, "aa:bb:cc:dd:ee:01", ops.deletes[0].MacAddress)
		assert.Equal(t, "aa:bb:cc:dd:ee:99", ops.insert.MacAddress)
		assert.Equal(t, compID, ops.insert.ComponentID)
	})

	t.Run("multiple host BMCs, keeper matches spec -> delete the extras", func(t *testing.T) {
		ip1 := "10.0.0.1"
		ip2 := "10.0.0.2"
		c := &model.Component{
			ID: compID,
			BMCs: []model.BMC{
				{MacAddress: "aa:bb:cc:dd:ee:01", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), IPAddress: &ip1, ComponentID: compID},
				{MacAddress: "aa:bb:cc:dd:ee:02", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), IPAddress: &ip2, ComponentID: compID},
			},
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:01", IPAddress: "10.0.0.1"})
		assert.Nil(t, ops.insert, "spec MAC matches a keeper, no insert needed")
		assert.Nil(t, ops.update, "IP on the keeper already matches")
		require.Len(t, ops.deletes, 1)
		assert.Equal(t, "aa:bb:cc:dd:ee:02", ops.deletes[0].MacAddress, "stale host BMC gets hard-deleted; Core says exactly one host BMC")
	})

	t.Run("multiple host BMCs, none match spec -> delete all + insert new", func(t *testing.T) {
		ip1 := "10.0.0.1"
		ip2 := "10.0.0.2"
		c := &model.Component{
			ID: compID,
			BMCs: []model.BMC{
				{MacAddress: "aa:bb:cc:dd:ee:01", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), IPAddress: &ip1, ComponentID: compID},
				{MacAddress: "aa:bb:cc:dd:ee:02", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), IPAddress: &ip2, ComponentID: compID},
			},
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:99", IPAddress: "10.0.0.9"})
		require.NotNil(t, ops.insert)
		assert.Equal(t, "aa:bb:cc:dd:ee:99", ops.insert.MacAddress)
		assert.Nil(t, ops.update)
		require.Len(t, ops.deletes, 2, "both stale host BMCs must go before the new one is inserted")
	})

	// The next three tests cover the DPU-coexistence case. A Compute
	// component in Flow can carry both a host BMC and a DPU BMC; bun's
	// .Relation("BMCs") preload has no stable ORDER BY so the DPU row may
	// land at BMCs[0]. The mirror MUST ignore non-host BMCs — Core's
	// ExpectedMachine doesn't describe the DPU BMC at all, so any op
	// the mirror takes against the DPU row would be data loss.

	dpuIP := "10.0.0.50"
	hostIP := "10.0.0.1"
	withDPUFirst := func(host model.BMC) []model.BMC {
		return []model.BMC{
			{MacAddress: "aa:bb:cc:dd:ee:50", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU), IPAddress: &dpuIP, ComponentID: compID},
			host,
		}
	}

	t.Run("dpu BMC at index 0, no host BMC -> insert host, dpu untouched", func(t *testing.T) {
		c := &model.Component{
			ID: compID,
			BMCs: []model.BMC{
				{MacAddress: "aa:bb:cc:dd:ee:50", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU), IPAddress: &dpuIP, ComponentID: compID},
			},
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:01", IPAddress: "10.0.0.1"})
		require.NotNil(t, ops.insert)
		assert.Equal(t, "aa:bb:cc:dd:ee:01", ops.insert.MacAddress)
		assert.Equal(t, devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), ops.insert.Type)
		assert.Empty(t, ops.deletes, "dpu BMC must not be deleted; Core has no opinion on it")
		assert.Nil(t, ops.update)
	})

	t.Run("dpu BMC at index 0 + host BMC same MAC -> no op, dpu untouched", func(t *testing.T) {
		c := &model.Component{
			ID: compID,
			BMCs: withDPUFirst(model.BMC{
				MacAddress:  "aa:bb:cc:dd:ee:01",
				Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
				IPAddress:   &hostIP,
				ComponentID: compID,
			}),
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:01", IPAddress: "10.0.0.1"})
		assert.Nil(t, ops.insert)
		assert.Nil(t, ops.update)
		assert.Empty(t, ops.deletes)
	})

	t.Run("dpu BMC at index 0 + host BMC with MAC change -> swap host only", func(t *testing.T) {
		c := &model.Component{
			ID: compID,
			BMCs: withDPUFirst(model.BMC{
				MacAddress:  "aa:bb:cc:dd:ee:01",
				Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
				IPAddress:   &hostIP,
				ComponentID: compID,
			}),
		}
		ops := planBMCReconciliation(c, expectedBMCSpec{MACAddress: "aa:bb:cc:dd:ee:02", IPAddress: "10.0.0.2"})
		require.Len(t, ops.deletes, 1)
		require.NotNil(t, ops.insert)
		assert.Equal(t, "aa:bb:cc:dd:ee:01", ops.deletes[0].MacAddress, "must delete the host BMC, never the DPU one")
		assert.Equal(t, devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), ops.deletes[0].Type)
		assert.Equal(t, "aa:bb:cc:dd:ee:02", ops.insert.MacAddress)
	})
}

func TestEqualOptionalString(t *testing.T) {
	s1 := "a"
	s2 := "b"
	s1b := "a"
	assert.True(t, equalOptionalString(nil, nil))
	assert.True(t, equalOptionalString(&s1, &s1b))
	assert.False(t, equalOptionalString(&s1, &s2))
	assert.False(t, equalOptionalString(nil, &s1))
	assert.False(t, equalOptionalString(&s1, nil))
}

func TestOptionalString(t *testing.T) {
	assert.Nil(t, optionalString(""))
	out := optionalString("x")
	require.NotNil(t, out)
	assert.Equal(t, "x", *out)
}

// Pull-guard tests for the 3 component types, mirroring TestPullExpectedRacks.

type errExpectedMachinesClient struct {
	nicoapi.Client
	err  error
	rows []nicoapi.ExpectedMachineDetail
}

func (c *errExpectedMachinesClient) GetAllExpectedMachineDetails(_ context.Context) ([]nicoapi.ExpectedMachineDetail, error) {
	if c.err != nil {
		return nil, c.err
	}
	return c.rows, nil
}

type errExpectedSwitchesClient struct {
	nicoapi.Client
	err  error
	rows []nicoapi.ExpectedSwitchDetail
}

func (c *errExpectedSwitchesClient) GetAllExpectedSwitchDetails(_ context.Context) ([]nicoapi.ExpectedSwitchDetail, error) {
	if c.err != nil {
		return nil, c.err
	}
	return c.rows, nil
}

type errExpectedPowerShelvesClient struct {
	nicoapi.Client
	err  error
	rows []nicoapi.ExpectedPowerShelfDetail
}

func (c *errExpectedPowerShelvesClient) GetAllExpectedPowerShelfDetails(_ context.Context) ([]nicoapi.ExpectedPowerShelfDetail, error) {
	if c.err != nil {
		return nil, c.err
	}
	return c.rows, nil
}

func TestPullExpectedMachines(t *testing.T) {
	ctx := context.Background()
	t.Run("rpc error -> rpcOK=false", func(t *testing.T) {
		_, ok := pullExpectedMachines(ctx, &errExpectedMachinesClient{Client: nicoapi.NewMockClient(), err: errors.New("boom")})
		assert.False(t, ok)
	})
	t.Run("empty success -> rpcOK=true (authoritative, caller deletes all)", func(t *testing.T) {
		rows, ok := pullExpectedMachines(ctx, &errExpectedMachinesClient{Client: nicoapi.NewMockClient()})
		assert.True(t, ok)
		assert.Empty(t, rows)
	})
	t.Run("populated -> rpcOK=true", func(t *testing.T) {
		rows, ok := pullExpectedMachines(ctx, &errExpectedMachinesClient{
			Client: nicoapi.NewMockClient(),
			rows:   []nicoapi.ExpectedMachineDetail{{ExpectedMachineID: "x"}},
		})
		assert.True(t, ok)
		assert.Len(t, rows, 1)
	})
}

func TestPullExpectedSwitches(t *testing.T) {
	ctx := context.Background()
	t.Run("rpc error -> rpcOK=false", func(t *testing.T) {
		_, ok := pullExpectedSwitches(ctx, &errExpectedSwitchesClient{Client: nicoapi.NewMockClient(), err: errors.New("boom")})
		assert.False(t, ok)
	})
	t.Run("empty success -> rpcOK=true", func(t *testing.T) {
		_, ok := pullExpectedSwitches(ctx, &errExpectedSwitchesClient{Client: nicoapi.NewMockClient()})
		assert.True(t, ok)
	})
}

func TestPullExpectedPowerShelves(t *testing.T) {
	ctx := context.Background()
	t.Run("rpc error -> rpcOK=false", func(t *testing.T) {
		_, ok := pullExpectedPowerShelves(ctx, &errExpectedPowerShelvesClient{Client: nicoapi.NewMockClient(), err: errors.New("boom")})
		assert.False(t, ok)
	})
	t.Run("empty success -> rpcOK=true", func(t *testing.T) {
		_, ok := pullExpectedPowerShelves(ctx, &errExpectedPowerShelvesClient{Client: nicoapi.NewMockClient()})
		assert.True(t, ok)
	})
}
