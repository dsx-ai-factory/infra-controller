// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicolegacy

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/common/dpureprov"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestConfigDecoderDecodeYAML(t *testing.T) {
	decoder := ConfigDecoder{}

	decoded, err := decoder.DecodeYAML(yaml.Node{})
	require.NoError(t, err)
	config := decoded.(*Config)
	assert.Equal(t, DefaultComputePowerDelay, config.ComputePowerDelay)

	decoded, err = decoder.DecodeYAML(managerYAMLNode(t, `compute_power_delay: 0s`))
	require.NoError(t, err)
	config = decoded.(*Config)
	assert.Equal(t, 0*time.Second, config.ComputePowerDelay)

	_, err = decoder.DecodeYAML(managerYAMLNode(t, `compute_power_delay: nope`))
	require.Error(t, err)
	assert.True(t, errors.Is(err, cmconfig.ErrInvalidManagerConfigField))
	assertInvalidManagerConfigField(t, err, "compute_power_delay")

	_, err = decoder.DecodeYAML(managerYAMLNode(t, `compute_power_dely: 15s`))
	require.Error(t, err)
	assert.True(t, errors.Is(err, cmconfig.ErrInvalidManagerConfig))
}

func TestInjectExpectation(t *testing.T) {
	testCases := map[string]struct {
		client      nicoapi.Client
		info        operations.InjectExpectationTaskInfo
		expectError bool
		errContains string
	}{
		"success": {
			client: nicoapi.NewMockClient(),
			info: operations.InjectExpectationTaskInfo{
				Info: mustMarshal(t, nicoapi.AddExpectedMachineRequest{
					BMCMACAddress:       "aa:bb:cc:dd:ee:ff",
					BMCUsername:         "admin",
					BMCPassword:         "password",
					ChassisSerialNumber: "SN12345",
				}),
			},
			expectError: false,
		},
		"invalid json returns error": {
			client: nicoapi.NewMockClient(),
			info: operations.InjectExpectationTaskInfo{
				Info: json.RawMessage(`{invalid`),
			},
			expectError: true,
			errContains: "failed to unmarshal",
		},
		"nil client returns error": {
			client: nil,
			info: operations.InjectExpectationTaskInfo{
				Info: mustMarshal(t, nicoapi.AddExpectedMachineRequest{
					BMCMACAddress: "aa:bb:cc:dd:ee:ff",
				}),
			},
			expectError: true,
			errContains: "nico client is not configured",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			m := New(tc.client, 0, nil)

			target := common.Target{
				Type:        devicetypes.ComponentTypeCompute,
				Identifiers: []string{"machine-1"},
			}

			err := m.InjectExpectation(context.Background(), target, tc.info)
			if tc.expectError {
				assert.Error(t, err)
				if tc.errContains != "" {
					assert.Contains(t, err.Error(), tc.errContains)
				}
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func mustMarshal(t *testing.T, v any) json.RawMessage {
	t.Helper()
	data, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("failed to marshal: %v", err)
	}
	return data
}

func assertInvalidManagerConfigField(t *testing.T, err error, field string) {
	t.Helper()

	var fieldErr cmconfig.InvalidManagerConfigFieldError
	require.True(t, errors.As(err, &fieldErr))
	assert.Equal(t, ConfigDecoder{}.Identity(), fieldErr.Identity)
	assert.Equal(t, field, fieldErr.Field)
}

func managerYAMLNode(t *testing.T, data string) yaml.Node {
	t.Helper()

	var node yaml.Node
	require.NoError(t, yaml.Unmarshal([]byte(data), &node))
	require.NotEmpty(t, node.Content)
	return *node.Content[0]
}

// TestFirmwareControl_SubTargetsAccepted verifies that the
// compute/nicolegacy FirmwareControl path tolerates info.SubTargets
// without erroring. This path goes through SetMachineAutoUpdate +
// SetFirmwareUpdateTimeWindow, which has no per-sub-target selection in
// NICo, so the manager only logs a warning and proceeds; we exercise
// that branch here. The replacement compute/nico implementation routes
// through Core's UpdateComponentFirmware which does honour sub-targets.
func TestFirmwareControlRejectsAuthenticationData(t *testing.T) {
	m := &Manager{}

	err := m.FirmwareControl(
		context.Background(),
		common.Target{},
		operations.FirmwareControlTaskInfo{AccessToken: "token"},
	)

	require.ErrorContains(t, err, "not supported by the nicolegacy compute manager")
}

func TestFirmwareControl_SubTargetsAccepted(t *testing.T) {
	tests := map[string]struct {
		subTargets []string
	}{
		"nil sub_targets (legacy path)":     {subTargets: nil},
		"empty sub_targets (legacy path)":   {subTargets: []string{}},
		"non-empty sub_targets (warn path)": {subTargets: []string{"bmc", "bios"}},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			m := New(nicoapi.NewMockClient(), 0, nil)
			target := common.Target{
				Type:        devicetypes.ComponentTypeCompute,
				Identifiers: []string{"machine-1"},
			}

			err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
				Operation:  operations.FirmwareOperationUpgrade,
				SubTargets: tc.subTargets,
			})
			require.NoError(t, err)
		})
	}
}

// TestFirmwareControl_DpuTarget_Nicolegacy pins the DPU branch on the
// legacy path: even though compute-tray-internal targets ignore
// per-sub-target selection on this path, the "dpu" target still routes
// through the dpureprov SAGA. Mixed targets run BMC scheduling first
// and DPU last; a DPU-only request skips the legacy compute-tray
// scheduling entirely so we don't accidentally enable auto-update on
// every machine.
func TestFirmwareControl_DpuTarget_Nicolegacy(t *testing.T) {
	tests := map[string]struct {
		subTargets       []string
		wantDpuTriggered bool
	}{
		"dpu-only opts in":      {subTargets: []string{"dpu"}, wantDpuTriggered: true},
		"mixed bmc+dpu opts in": {subTargets: []string{"bmc", "dpu"}, wantDpuTriggered: true},
		"empty opts out":        {subTargets: nil, wantDpuTriggered: false},
		"bmc only opts out":     {subTargets: []string{"bmc"}, wantDpuTriggered: false},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			client := nicoapi.NewMockClient()
			client.SetHostDpuMachineIds("machine-1", []string{"dpu-1"})
			client.SetHostInstanceID("machine-1", "inst-1")

			m := withFastDpuReprovLegacy(New(client, 0, nil), client, "machine-1")
			target := common.Target{
				Type:        devicetypes.ComponentTypeCompute,
				Identifiers: []string{"machine-1"},
			}

			err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
				Operation:  operations.FirmwareOperationUpgrade,
				SubTargets: tc.subTargets,
			})
			require.NoError(t, err)

			if tc.wantDpuTriggered {
				require.Len(t, client.DpuReprovisioningTriggers(), 1,
					"DPU branch must run when targets contains 'dpu'")
				assert.Empty(t, client.HostUpdateOverridesActive(),
					"override must be removed via deferred cleanup")
			} else {
				assert.Empty(t, client.DpuReprovisioningTriggers(),
					"DPU branch must NOT run unless targets contains 'dpu'")
			}
		})
	}
}

// withFastDpuReprovLegacy is the nicolegacy counterpart of
// compute/nico's withFastDpuReprov: it shortcircuits the dpureprov
// poll loop so DPU-branch tests do not hit the production 30s cadence.
func withFastDpuReprovLegacy(m *Manager, mock nicoapi.Client, hostID string) *Manager {
	pollIdx := 0
	m.dpuReprovOpts = dpureprov.Options{
		PollInterval: 1 * time.Microsecond,
		PollTimeout:  10 * time.Second,
		Sleep: func(_ context.Context, _ time.Duration) error {
			pollIdx++
			if pollIdx == 1 {
				mock.SetDpuReprovisioningPending(hostID, false)
			}
			return nil
		},
	}
	return m
}

// --- Tests for firmware version helper functions ---

func desiredEntry(versions map[string]string) *corev1.DesiredFirmwareVersionEntry {
	return &corev1.DesiredFirmwareVersionEntry{
		ComponentVersions: versions,
	}
}

func TestVersionsEqual(t *testing.T) {
	tests := map[string]struct {
		a, b   map[string]string
		expect bool
	}{
		"equal single key": {
			a:      map[string]string{"bmc": "1.0"},
			b:      map[string]string{"bmc": "1.0"},
			expect: true,
		},
		"equal multiple keys": {
			a:      map[string]string{"bmc": "1.0", "uefi": "2.0"},
			b:      map[string]string{"bmc": "1.0", "uefi": "2.0"},
			expect: true,
		},
		"different values": {
			a:      map[string]string{"bmc": "1.0"},
			b:      map[string]string{"bmc": "2.0"},
			expect: false,
		},
		"different lengths": {
			a:      map[string]string{"bmc": "1.0"},
			b:      map[string]string{"bmc": "1.0", "uefi": "2.0"},
			expect: false,
		},
		"both empty": {
			a:      map[string]string{},
			b:      map[string]string{},
			expect: true,
		},
		"a nil b empty": {
			a:      nil,
			b:      map[string]string{},
			expect: true,
		},
		"both nil": {
			a:      nil,
			b:      nil,
			expect: true,
		},
		"missing key in b": {
			a:      map[string]string{"bmc": "1.0", "uefi": "2.0"},
			b:      map[string]string{"bmc": "1.0", "cpld": "3.0"},
			expect: false,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.expect, versionsEqual(tc.a, tc.b))
		})
	}
}

func TestFirmwareVersionsMatch(t *testing.T) {
	tests := map[string]struct {
		desired, actual map[string]string
		expect          bool
	}{
		"exact match": {
			desired: map[string]string{"bmc": "1.0", "uefi": "2.0"},
			actual:  map[string]string{"bmc": "1.0", "uefi": "2.0"},
			expect:  true,
		},
		"desired is subset of actual": {
			desired: map[string]string{"bmc": "1.0"},
			actual:  map[string]string{"bmc": "1.0", "uefi": "2.0", "cpld": "3.0"},
			expect:  true,
		},
		"version mismatch": {
			desired: map[string]string{"bmc": "1.0"},
			actual:  map[string]string{"bmc": "2.0"},
			expect:  false,
		},
		"desired key missing from actual": {
			desired: map[string]string{"bmc": "1.0", "uefi": "2.0"},
			actual:  map[string]string{"bmc": "1.0"},
			expect:  false,
		},
		"empty desired returns false": {
			desired: map[string]string{},
			actual:  map[string]string{"bmc": "1.0"},
			expect:  false,
		},
		"nil desired returns false": {
			desired: nil,
			actual:  map[string]string{"bmc": "1.0"},
			expect:  false,
		},
		"empty actual with non-empty desired returns false": {
			desired: map[string]string{"bmc": "1.0"},
			actual:  map[string]string{},
			expect:  false,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.expect, firmwareVersionsMatch(tc.desired, tc.actual))
		})
	}
}

func TestMatchesAnyDesired(t *testing.T) {
	tests := map[string]struct {
		actual  map[string]string
		entries []*corev1.DesiredFirmwareVersionEntry
		expect  bool
	}{
		"matches first entry": {
			actual: map[string]string{"bmc": "1.0", "uefi": "2.0"},
			entries: []*corev1.DesiredFirmwareVersionEntry{
				desiredEntry(map[string]string{"bmc": "1.0"}),
				desiredEntry(map[string]string{"bmc": "9.0"}),
			},
			expect: true,
		},
		"matches second entry": {
			actual: map[string]string{"bmc": "9.0"},
			entries: []*corev1.DesiredFirmwareVersionEntry{
				desiredEntry(map[string]string{"bmc": "1.0"}),
				desiredEntry(map[string]string{"bmc": "9.0"}),
			},
			expect: true,
		},
		"matches none": {
			actual: map[string]string{"bmc": "5.0"},
			entries: []*corev1.DesiredFirmwareVersionEntry{
				desiredEntry(map[string]string{"bmc": "1.0"}),
				desiredEntry(map[string]string{"bmc": "9.0"}),
			},
			expect: false,
		},
		"empty entries": {
			actual:  map[string]string{"bmc": "1.0"},
			entries: nil,
			expect:  false,
		},
		"entry with empty component_versions never matches": {
			actual: map[string]string{"bmc": "1.0"},
			entries: []*corev1.DesiredFirmwareVersionEntry{
				desiredEntry(map[string]string{}),
			},
			expect: false,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.expect, matchesAnyDesired(tc.actual, tc.entries))
		})
	}
}

func TestParseTargetVersion(t *testing.T) {
	tests := map[string]struct {
		input       string
		expected    map[string]string
		expectError bool
		errContains string
	}{
		"valid json object": {
			input:    `{"bmc":"7.10.30.00","uefi":"2.22.1"}`,
			expected: map[string]string{"bmc": "7.10.30.00", "uefi": "2.22.1"},
		},
		"single key": {
			input:    `{"bmc":"1.0"}`,
			expected: map[string]string{"bmc": "1.0"},
		},
		"empty object": {
			input:    `{}`,
			expected: map[string]string{},
		},
		"invalid json": {
			input:       `{not valid`,
			expectError: true,
			errContains: "target_version must be a JSON object",
		},
		"json array instead of object": {
			input:       `["bmc","1.0"]`,
			expectError: true,
			errContains: "target_version must be a JSON object",
		},
		"json string instead of object": {
			input:       `"bmc:1.0"`,
			expectError: true,
			errContains: "target_version must be a JSON object",
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			result, err := parseTargetVersion(tc.input)
			if tc.expectError {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
			} else {
				require.NoError(t, err)
				assert.Equal(t, tc.expected, result)
			}
		})
	}
}

func TestIsTargetVersionInDesired(t *testing.T) {
	entries := []*corev1.DesiredFirmwareVersionEntry{
		desiredEntry(map[string]string{"bmc": "7.10.30.00", "uefi": "2.22.1"}),
		desiredEntry(map[string]string{"bmc": "8.0.0.00", "uefi": "3.0.0"}),
	}

	tests := map[string]struct {
		target  map[string]string
		entries []*corev1.DesiredFirmwareVersionEntry
		expect  bool
	}{
		"matches first entry exactly": {
			target:  map[string]string{"bmc": "7.10.30.00", "uefi": "2.22.1"},
			entries: entries,
			expect:  true,
		},
		"matches second entry exactly": {
			target:  map[string]string{"bmc": "8.0.0.00", "uefi": "3.0.0"},
			entries: entries,
			expect:  true,
		},
		"partial match is not equal": {
			target:  map[string]string{"bmc": "7.10.30.00"},
			entries: entries,
			expect:  false,
		},
		"no match": {
			target:  map[string]string{"bmc": "99.0.0", "uefi": "99.0"},
			entries: entries,
			expect:  false,
		},
		"empty entries": {
			target:  map[string]string{"bmc": "1.0"},
			entries: nil,
			expect:  false,
		},
		"empty target with empty entry": {
			target:  map[string]string{},
			entries: []*corev1.DesiredFirmwareVersionEntry{desiredEntry(map[string]string{})},
			expect:  true,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.expect, isTargetVersionInDesired(tc.target, tc.entries))
		})
	}
}

func TestAllFirmwareUpToDate(t *testing.T) {
	desiredEntries := []*corev1.DesiredFirmwareVersionEntry{
		desiredEntry(map[string]string{"bmc": "1.0", "uefi": "2.0"}),
		desiredEntry(map[string]string{"bmc": "3.0", "uefi": "4.0"}),
	}

	tests := map[string]struct {
		componentIDs   []string
		actualFirmware map[string]map[string]string
		targetFirmware map[string]string
		desiredEntries []*corev1.DesiredFirmwareVersionEntry
		expect         bool
	}{
		"all match target firmware": {
			componentIDs: []string{"m1", "m2"},
			actualFirmware: map[string]map[string]string{
				"m1": {"bmc": "1.0", "uefi": "2.0"},
				"m2": {"bmc": "1.0", "uefi": "2.0"},
			},
			targetFirmware: map[string]string{"bmc": "1.0"},
			desiredEntries: desiredEntries,
			expect:         true,
		},
		"one machine does not match target": {
			componentIDs: []string{"m1", "m2"},
			actualFirmware: map[string]map[string]string{
				"m1": {"bmc": "1.0", "uefi": "2.0"},
				"m2": {"bmc": "OLD", "uefi": "2.0"},
			},
			targetFirmware: map[string]string{"bmc": "1.0"},
			desiredEntries: desiredEntries,
			expect:         false,
		},
		"all match desired (no target)": {
			componentIDs: []string{"m1", "m2"},
			actualFirmware: map[string]map[string]string{
				"m1": {"bmc": "1.0", "uefi": "2.0"},
				"m2": {"bmc": "3.0", "uefi": "4.0"},
			},
			targetFirmware: nil,
			desiredEntries: desiredEntries,
			expect:         true,
		},
		"one machine does not match any desired": {
			componentIDs: []string{"m1", "m2"},
			actualFirmware: map[string]map[string]string{
				"m1": {"bmc": "1.0", "uefi": "2.0"},
				"m2": {"bmc": "OLD", "uefi": "OLD"},
			},
			targetFirmware: nil,
			desiredEntries: desiredEntries,
			expect:         false,
		},
		"empty actualFirmware": {
			componentIDs:   []string{"m1"},
			actualFirmware: map[string]map[string]string{},
			targetFirmware: nil,
			desiredEntries: desiredEntries,
			expect:         false,
		},
		"nil actualFirmware": {
			componentIDs:   []string{"m1"},
			actualFirmware: nil,
			targetFirmware: nil,
			desiredEntries: desiredEntries,
			expect:         false,
		},
		"component missing from actualFirmware": {
			componentIDs: []string{"m1", "m2"},
			actualFirmware: map[string]map[string]string{
				"m1": {"bmc": "1.0", "uefi": "2.0"},
			},
			targetFirmware: nil,
			desiredEntries: desiredEntries,
			expect:         false,
		},
		"component has empty firmware map": {
			componentIDs: []string{"m1"},
			actualFirmware: map[string]map[string]string{
				"m1": {},
			},
			targetFirmware: nil,
			desiredEntries: desiredEntries,
			expect:         false,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.expect, allFirmwareUpToDate(
				tc.componentIDs, tc.actualFirmware, tc.targetFirmware, tc.desiredEntries,
			))
		})
	}
}

// newManagerForReadinessTest builds a Manager with a tight-timeout
// readiness gate backed by the supplied MemReader so the wait loop
// actually times out within the test budget. The caller seeds the
// reader with the ComponentOperationStatus rows the test expects.
func newManagerForReadinessTest(t *testing.T, client nicoapi.Client, reader *readiness.MemReader) *Manager {
	t.Helper()
	gate := readiness.NewDBGate(reader, 50*time.Millisecond, 10*time.Millisecond)
	return New(client, 0, gate)
}

// inUseStatus returns a status that blocks every disruptive operation,
// mirroring what inventorysync would persist for a tenant-attached host.
func inUseStatus() *types.ComponentOperationStatus {
	return &types.ComponentOperationStatus{
		Phase:  types.PhaseInUse,
		Reason: "tenant attached",
		BlockedOperations: []types.OperationType{
			types.OperationTypePowerControl,
			types.OperationTypeFirmwareControl,
		},
	}
}

func TestPowerControl_RefusesInUseMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"machine-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOff,
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
	assert.Contains(t, err.Error(), "machine-1")
}

func TestPowerControl_AllowsReadyMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", &types.ComponentOperationStatus{Phase: types.PhaseReady})

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"machine-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	require.NoError(t, err)
}

func TestBringUpControl_RefusesInUseMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"machine-1"},
	}

	err := m.BringUpControl(context.Background(), target, operations.BringUpTaskInfo{})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
}

func TestFirmwareControl_RefusesInUseMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"machine-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "1.0.0",
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
}

// TestPowerControl_OverrideBypassesReadinessCheck verifies that the
// operator-controlled OverrideReadinessCheck flag short-circuits the
// readiness gate on PowerControl. The target host is reported as in-use
// — which would otherwise block the call — yet the operation is
// expected to proceed past the gate.
func TestPowerControl_OverrideBypassesReadinessCheck(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"machine-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation:              operations.PowerOperationPowerOn,
		OverrideReadinessCheck: true,
	})
	require.NoError(t, err)
}

// TestBringUpControl_OverrideBypassesReadinessCheck is the BringUp
// counterpart of TestPowerControl_OverrideBypassesReadinessCheck.
func TestBringUpControl_OverrideBypassesReadinessCheck(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"machine-1"},
	}

	err := m.BringUpControl(context.Background(), target, operations.BringUpTaskInfo{
		OverrideReadinessCheck: true,
	})
	require.NoError(t, err)
}
