// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	"errors"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type mockClient struct {
	machines                    map[string]MachineDetail
	powerStates                 map[string]PowerState
	machineInterfaces           map[string]MachineInterface
	expectedSwitches            map[string]ExpectedSwitchInfo // keyed by BMC MAC
	leakingMachineIds           []string
	leakingSwitchIds            []string
	firmwareUpdateTimeWindowErr error // If set, SetFirmwareUpdateTimeWindow will return this error
	adminPowerControlErr        error // If set, AdminPowerControl will return this error
	desiredFirmwareVersions     []*corev1.DesiredFirmwareVersionEntry
	lastUpdateFirmwareRequest   *corev1.UpdateComponentFirmwareRequest
	// Topology lookups exercised by the rack-assignment safety check. Tests
	// populate these via Set...RackId / Set...HostMachineIds helpers.
	switchRackIDs              map[string]string // switch ID → rack ID
	powerShelfRackIDs          map[string]string // power shelf ID → rack ID
	switchControllerStates     map[string]string // switch ID → raw core controller_state
	switchNvosIPs              map[string]string // switch ID → resolved NVOS host IP
	nvLinkDomainMemberships    []NVLinkDomainMembership
	powerShelfControllerStates map[string]string // shelf ID → raw core controller_state
	observedSwitches           []ObservedControllerDevice
	observedPowerShelves       []ObservedControllerDevice
	hostMachinesByRackID       map[string][]string
	// Detail tables for the GetAllExpected*Details RPCs (Flow's mirror sync).
	// Keyed by the natural identifier the test cares about so test helpers can
	// overwrite individual entries without rebuilding the whole slice.
	expectedRackDetails       map[string]ExpectedRackDetail       // by RackID
	expectedMachineDetails    map[string]ExpectedMachineDetail    // by ExpectedMachineID (UUID)
	expectedSwitchDetails     map[string]ExpectedSwitchDetail     // by ExpectedSwitchID (UUID)
	expectedPowerShelfDetails map[string]ExpectedPowerShelfDetail // by ExpectedPowerShelfID (UUID)

	// Decommission mock state.
	machineControllerStates   map[string]string // machine ID → raw state string
	decommissionMachineErr    error             // returned by DecommissionMachine if set
	decommissionSwitchErr     error             // returned by DecommissionSwitch if set
	decommissionPowerShelfErr error             // returned by DecommissionPowerShelf if set

	// DPU reprovisioning mock state. Populated by tests via the
	// SetDpu... helpers. The mock keeps the list of pending DPUs by host
	// id and the configured (host -> instance) and (host -> dpu ids)
	// mappings independently so a test can mix-and-match e.g. a host
	// without an instance but with DPUs.
	pendingDpuReprovHosts         map[string]bool         // host machine id -> still pending
	hostToDpuMachineIds           map[string][]string     // host machine id -> dpu machine ids
	hostToInstanceID              map[string]string       // host machine id -> instance id ("" if none)
	hostUpdateInProgressOverrides map[string]string       // host machine id -> last "message"
	dpuReprovisioningTriggers     []DpuReprovisioningCall // recorded TriggerDpuReprovisioning calls
	instancePowerCalls            []InstancePowerCall     // recorded InvokeInstancePower calls
	insertHostUpdateOverrideErr   error                   // if set, InsertHostUpdateInProgressHealthOverride returns this
	triggerDpuReprovisioningErr   error                   // if set, TriggerDpuReprovisioning returns this
	invokeInstancePowerErr        error                   // if set, InvokeInstancePower returns this
}

// MockClient is a Client with accessors for requests captured by the in-memory
// test implementation.
type MockClient interface {
	Client
	LastUpdateComponentFirmwareRequest() *corev1.UpdateComponentFirmwareRequest
}

// DpuReprovisioningCall captures a TriggerDpuReprovisioning invocation
// for assertion in tests.
type DpuReprovisioningCall struct {
	MachineID      string
	UpdateFirmware bool
}

// InstancePowerCall captures an InvokeInstancePower invocation for
// assertion in tests.
type InstancePowerCall struct {
	InstanceID   string
	ApplyUpdates bool
}

// Close is a no-op because the mock owns no connection or certificate watcher.
func (c *mockClient) Close() error { return nil }

// NewMockClient returns a "GRPC" client that returns mock values so it can be used in unit tests.
func NewMockClient() MockClient {
	return &mockClient{
		machines:                      map[string]MachineDetail{},
		powerStates:                   map[string]PowerState{},
		machineInterfaces:             map[string]MachineInterface{},
		expectedSwitches:              map[string]ExpectedSwitchInfo{},
		switchRackIDs:                 map[string]string{},
		powerShelfRackIDs:             map[string]string{},
		switchControllerStates:        map[string]string{},
		switchNvosIPs:                 map[string]string{},
		powerShelfControllerStates:    map[string]string{},
		hostMachinesByRackID:          map[string][]string{},
		expectedRackDetails:           map[string]ExpectedRackDetail{},
		expectedMachineDetails:        map[string]ExpectedMachineDetail{},
		expectedSwitchDetails:         map[string]ExpectedSwitchDetail{},
		expectedPowerShelfDetails:     map[string]ExpectedPowerShelfDetail{},
		machineControllerStates:       map[string]string{},
		pendingDpuReprovHosts:         map[string]bool{},
		hostToDpuMachineIds:           map[string][]string{},
		hostToInstanceID:              map[string]string{},
		hostUpdateInProgressOverrides: map[string]string{},
	}
}

func (c *mockClient) Version(ctx context.Context) (string, error) {
	return "1.2.3", nil
}

func (c *mockClient) GetMachines(ctx context.Context) ([]MachineDetail, error) {
	var result []MachineDetail
	for _, m := range c.machines {
		result = append(result, m)
	}
	return result, nil
}

func (c *mockClient) GetLeakingMachineIds(ctx context.Context) ([]string, error) {
	return c.leakingMachineIds, nil
}

func (c *mockClient) GetLeakingSwitchIds(ctx context.Context) ([]string, error) {
	return c.leakingSwitchIds, nil
}

func (c *mockClient) SetLeakingMachineIds(ids []string) {
	c.leakingMachineIds = ids
}

func (c *mockClient) SetLeakingSwitchIds(ids []string) {
	c.leakingSwitchIds = ids
}

func (c *mockClient) GetPowerStates(ctx context.Context, machineIds []string) (ret []MachinePowerState, err error) {
	for _, cur := range machineIds {
		if state, ok := c.powerStates[cur]; ok {
			ret = append(ret, MachinePowerState{MachineID: cur, PowerState: state})
		}
	}

	return ret, nil
}

func (c *mockClient) SetFirmwareUpdateTimeWindow(ctx context.Context, machineIds []string, startTime, endTime time.Time) error {
	return c.firmwareUpdateTimeWindowErr
}

func (c *mockClient) AdminPowerControl(ctx context.Context, machineID string, action SystemPowerControl) error {
	return c.adminPowerControlErr
}

func (c *mockClient) UpdatePowerOption(ctx context.Context, machineID string, desiredState PowerState) error {
	return nil
}

func (c *mockClient) AddMachine(machine MachineDetail) {
	c.machines[machine.MachineID] = machine
}

func (c *mockClient) AddPowerState(machineID string, state PowerState) {
	c.powerStates[machineID] = state
}

func (c *mockClient) SetFirmwareUpdateTimeWindowError(err error) {
	c.firmwareUpdateTimeWindowErr = err
}

func (c *mockClient) SetAdminPowerControlError(err error) {
	c.adminPowerControlErr = err
}

func (c *mockClient) FindInterfaces(ctx context.Context) (map[string]MachineInterface, error) {
	interfaces := make(map[string]MachineInterface)
	for mac, iface := range c.machineInterfaces {
		interfaces[mac] = iface
	}
	return interfaces, nil
}

func (c *mockClient) AddMachineInterface(iface MachineInterface) {
	c.machineInterfaces[utils.NormalizeMAC(iface.MacAddress)] = iface
}

func (c *mockClient) FindMachinesByIds(ctx context.Context, machineIds []string) ([]MachineDetail, error) {
	var result []MachineDetail
	for _, id := range machineIds {
		if m, ok := c.machines[id]; ok {
			result = append(result, m)
		}
	}
	return result, nil
}

func (c *mockClient) FindHostMachineIdsByRack(_ context.Context, rackID string) ([]string, error) {
	if rackID == "" {
		return nil, errors.New("rack ID is required")
	}
	ids := c.hostMachinesByRackID[rackID]
	if len(ids) == 0 {
		return nil, nil
	}
	out := make([]string, len(ids))
	copy(out, ids)
	return out, nil
}

func (c *mockClient) FindSwitchRackIDs(_ context.Context, switchIds []string) (map[string]string, error) {
	if len(switchIds) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(switchIds))
	for _, id := range switchIds {
		if rid, ok := c.switchRackIDs[id]; ok && rid != "" {
			out[id] = rid
		}
	}
	return out, nil
}

func (c *mockClient) FindPowerShelfRackIDs(_ context.Context, shelfIds []string) (map[string]string, error) {
	if len(shelfIds) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(shelfIds))
	for _, id := range shelfIds {
		if rid, ok := c.powerShelfRackIDs[id]; ok && rid != "" {
			out[id] = rid
		}
	}
	return out, nil
}

// SetSwitchRackID records the rack assignment for a switch (mock only).
func (c *mockClient) SetSwitchRackID(switchID, rackID string) {
	c.switchRackIDs[switchID] = rackID
}

// SetPowerShelfRackID records the rack assignment for a power shelf (mock only).
func (c *mockClient) SetPowerShelfRackID(shelfID, rackID string) {
	c.powerShelfRackIDs[shelfID] = rackID
}

func (c *mockClient) FindSwitchControllerStates(_ context.Context, switchIds []string) (map[string]string, error) {
	if len(switchIds) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(switchIds))
	for _, id := range switchIds {
		if s, ok := c.switchControllerStates[id]; ok && s != "" {
			out[id] = s
		}
	}
	return out, nil
}

func (c *mockClient) FindSwitchNvosIPs(_ context.Context, switchIds []string) (map[string]string, error) {
	if len(switchIds) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(switchIds))
	for _, id := range switchIds {
		if ip, ok := c.switchNvosIPs[id]; ok && ip != "" {
			out[id] = ip
		}
	}
	return out, nil
}

func (c *mockClient) GetObservedNVLinkDomainMemberships(_ context.Context) ([]NVLinkDomainMembership, error) {
	out := make([]NVLinkDomainMembership, len(c.nvLinkDomainMemberships))
	copy(out, c.nvLinkDomainMemberships)
	return out, nil
}

// SetObservedNVLinkDomainMemberships replaces the Core domain observations
// returned by the mock client.
func (c *mockClient) SetObservedNVLinkDomainMemberships(memberships []NVLinkDomainMembership) {
	c.nvLinkDomainMemberships = make([]NVLinkDomainMembership, len(memberships))
	copy(c.nvLinkDomainMemberships, memberships)
}

func (c *mockClient) FindPowerShelfControllerStates(_ context.Context, shelfIds []string) (map[string]string, error) {
	if len(shelfIds) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(shelfIds))
	for _, id := range shelfIds {
		if s, ok := c.powerShelfControllerStates[id]; ok && s != "" {
			out[id] = s
		}
	}
	return out, nil
}

// SetSwitchControllerState records the raw controller_state Core reports for a
// switch (mock only).
func (c *mockClient) SetSwitchControllerState(switchID, state string) {
	c.switchControllerStates[switchID] = state
}

// SetSwitchNvosIP records the resolved NVOS host IP Core reports for a switch
// (mock only).
func (c *mockClient) SetSwitchNvosIP(switchID, ip string) {
	c.switchNvosIPs[switchID] = ip
}

// SetPowerShelfControllerState records the raw controller_state Core reports
// for a power shelf (mock only).
func (c *mockClient) SetPowerShelfControllerState(shelfID, state string) {
	c.powerShelfControllerStates[shelfID] = state
}

// SetRackHostMachineIDs records which host machines a rack contains (mock only).
func (c *mockClient) SetRackHostMachineIDs(rackID string, machineIDs []string) {
	out := make([]string, len(machineIDs))
	copy(out, machineIDs)
	c.hostMachinesByRackID[rackID] = out
}

func (c *mockClient) GetMachinePositionInfo(ctx context.Context, machineIds []string) ([]MachinePosition, error) {
	// Mock implementation returns empty for now
	return nil, nil
}

func (c *mockClient) AllowIngestionAndPowerOn(
	ctx context.Context,
	bmcIP string,
	bmcMAC string,
) error {
	return nil
}

func (c *mockClient) DetermineMachineIngestionState(
	ctx context.Context,
	bmcIP string,
	bmcMAC string,
) (BringUpState, error) {
	return BringUpStateMachineCreated, nil
}

func (c *mockClient) AddExpectedMachine(ctx context.Context, req AddExpectedMachineRequest) error {
	return nil
}

func (c *mockClient) GetAllExpectedSwitches(_ context.Context) (map[string]ExpectedSwitchInfo, error) {
	results := make(map[string]ExpectedSwitchInfo)
	for mac, es := range c.expectedSwitches {
		results[mac] = es
	}
	return results, nil
}

func (c *mockClient) AddExpectedSwitch(ctx context.Context, req AddExpectedSwitchRequest) error {
	return nil
}

func (c *mockClient) AddExpectedPowerShelf(ctx context.Context, req AddExpectedPowerShelfRequest) error {
	return nil
}

func (c *mockClient) InsertHealthReportOverride(ctx context.Context, machineID string, source string) error {
	return nil
}

func (c *mockClient) RemoveHealthReportOverride(ctx context.Context, machineID string, source string) error {
	return nil
}

func (c *mockClient) ComponentPowerControl(ctx context.Context, req *corev1.ComponentPowerControlRequest) (*corev1.ComponentPowerControlResponse, error) {
	return &corev1.ComponentPowerControlResponse{}, nil
}

func (c *mockClient) UpdateComponentFirmware(ctx context.Context, req *corev1.UpdateComponentFirmwareRequest) (*corev1.UpdateComponentFirmwareResponse, error) {
	c.lastUpdateFirmwareRequest = req
	return &corev1.UpdateComponentFirmwareResponse{}, nil
}

// LastUpdateComponentFirmwareRequest returns the most recent firmware update
// request received by this mock.
func (c *mockClient) LastUpdateComponentFirmwareRequest() *corev1.UpdateComponentFirmwareRequest {
	return c.lastUpdateFirmwareRequest
}

func (c *mockClient) GetComponentFirmwareStatus(ctx context.Context, req *corev1.GetComponentFirmwareStatusRequest) (*corev1.GetComponentFirmwareStatusResponse, error) {
	return &corev1.GetComponentFirmwareStatusResponse{}, nil
}

func (c *mockClient) ListComponentFirmwareVersions(ctx context.Context, req *corev1.ListComponentFirmwareVersionsRequest) (*corev1.ListComponentFirmwareVersionsResponse, error) {
	return &corev1.ListComponentFirmwareVersionsResponse{}, nil
}

func (c *mockClient) GetComponentInventory(ctx context.Context, req *corev1.GetComponentInventoryRequest) (*corev1.GetComponentInventoryResponse, error) {
	return &corev1.GetComponentInventoryResponse{}, nil
}

func (c *mockClient) GetAllExpectedSwitchesLinked(_ context.Context) ([]LinkedExpectedSwitch, error) {
	return nil, nil
}

func (c *mockClient) GetAllExpectedPowerShelvesLinked(_ context.Context) ([]LinkedExpectedPowerShelf, error) {
	return nil, nil
}

func (c *mockClient) GetSwitches(_ context.Context) ([]ObservedControllerDevice, error) {
	return append([]ObservedControllerDevice(nil), c.observedSwitches...), nil
}

func (c *mockClient) GetPowerShelves(_ context.Context) ([]ObservedControllerDevice, error) {
	return append([]ObservedControllerDevice(nil), c.observedPowerShelves...), nil
}

func (c *mockClient) SetObservedSwitches(devices []ObservedControllerDevice) {
	c.observedSwitches = append([]ObservedControllerDevice(nil), devices...)
}

func (c *mockClient) SetObservedPowerShelves(devices []ObservedControllerDevice) {
	c.observedPowerShelves = append([]ObservedControllerDevice(nil), devices...)
}

func (c *mockClient) GetDesiredFirmwareVersions(_ context.Context) ([]*corev1.DesiredFirmwareVersionEntry, error) {
	return c.desiredFirmwareVersions, nil
}

func (c *mockClient) FindExploredEndpointsByIds(_ context.Context, _ []string) ([]*corev1.ExploredEndpoint, error) {
	return nil, nil
}

func (c *mockClient) SetMachineAutoUpdate(_ context.Context, _ string, _ bool) error {
	return nil
}

func (c *mockClient) AddExpectedSwitchInfo(info ExpectedSwitchInfo) {
	c.expectedSwitches[utils.NormalizeMAC(info.BMCMACAddress)] = info
}

func (c *mockClient) GetAllExpectedRackDetails(_ context.Context) ([]ExpectedRackDetail, error) {
	if len(c.expectedRackDetails) == 0 {
		return nil, nil
	}
	out := make([]ExpectedRackDetail, 0, len(c.expectedRackDetails))
	for _, d := range c.expectedRackDetails {
		out = append(out, d)
	}
	return out, nil
}

func (c *mockClient) GetAllExpectedMachineDetails(_ context.Context) ([]ExpectedMachineDetail, error) {
	if len(c.expectedMachineDetails) == 0 {
		return nil, nil
	}
	out := make([]ExpectedMachineDetail, 0, len(c.expectedMachineDetails))
	for _, d := range c.expectedMachineDetails {
		out = append(out, d)
	}
	return out, nil
}

func (c *mockClient) GetAllExpectedSwitchDetails(_ context.Context) ([]ExpectedSwitchDetail, error) {
	if len(c.expectedSwitchDetails) == 0 {
		return nil, nil
	}
	out := make([]ExpectedSwitchDetail, 0, len(c.expectedSwitchDetails))
	for _, d := range c.expectedSwitchDetails {
		out = append(out, d)
	}
	return out, nil
}

func (c *mockClient) GetAllExpectedPowerShelfDetails(_ context.Context) ([]ExpectedPowerShelfDetail, error) {
	if len(c.expectedPowerShelfDetails) == 0 {
		return nil, nil
	}
	out := make([]ExpectedPowerShelfDetail, 0, len(c.expectedPowerShelfDetails))
	for _, d := range c.expectedPowerShelfDetails {
		out = append(out, d)
	}
	return out, nil
}

// AddExpectedRackDetail registers an expected rack for the mock GetAllExpectedRackDetails call.
func (c *mockClient) AddExpectedRackDetail(detail ExpectedRackDetail) {
	c.expectedRackDetails[detail.RackID] = detail
}

// AddExpectedMachineDetail registers an expected machine for the mock
// GetAllExpectedMachineDetails call. Tests that don't care about the
// ExpectedMachineID may leave it empty; the map then uses "" as the key (only
// one such entry will survive).
func (c *mockClient) AddExpectedMachineDetail(detail ExpectedMachineDetail) {
	c.expectedMachineDetails[detail.ExpectedMachineID] = detail
}

// AddExpectedSwitchDetail registers an expected switch for the mock
// GetAllExpectedSwitchDetails call.
func (c *mockClient) AddExpectedSwitchDetail(detail ExpectedSwitchDetail) {
	c.expectedSwitchDetails[detail.ExpectedSwitchID] = detail
}

// AddExpectedPowerShelfDetail registers an expected power shelf for the mock
// GetAllExpectedPowerShelfDetails call.
func (c *mockClient) AddExpectedPowerShelfDetail(detail ExpectedPowerShelfDetail) {
	c.expectedPowerShelfDetails[detail.ExpectedPowerShelfID] = detail
}

// === DPU reprovisioning mock surface =====================================
//
// Mocks here record incoming requests so tests can assert ordering /
// call counts without owning the mock client struct. The companion
// SetDpu... helpers seed test fixtures.

func (c *mockClient) InsertHostUpdateInProgressHealthOverride(
	_ context.Context, machineID string, message string,
) error {
	if c.insertHostUpdateOverrideErr != nil {
		return c.insertHostUpdateOverrideErr
	}
	c.hostUpdateInProgressOverrides[machineID] = message
	return nil
}

func (c *mockClient) RemoveHostUpdateInProgressHealthOverride(
	_ context.Context, machineID string,
) error {
	delete(c.hostUpdateInProgressOverrides, machineID)
	return nil
}

func (c *mockClient) TriggerDpuReprovisioning(
	_ context.Context, machineID string, updateFirmware bool,
) error {
	if c.triggerDpuReprovisioningErr != nil {
		return c.triggerDpuReprovisioningErr
	}
	c.dpuReprovisioningTriggers = append(c.dpuReprovisioningTriggers, DpuReprovisioningCall{
		MachineID:      machineID,
		UpdateFirmware: updateFirmware,
	})
	c.pendingDpuReprovHosts[machineID] = true
	return nil
}

func (c *mockClient) IsDpuReprovisioningPendingForHost(
	_ context.Context, hostMachineID string,
) (bool, error) {
	return c.pendingDpuReprovHosts[hostMachineID], nil
}

func (c *mockClient) FindAssociatedDpuMachineIds(
	_ context.Context, hostMachineID string,
) ([]string, error) {
	if hostMachineID == "" {
		return nil, errors.New("host machine id is required")
	}
	ids := c.hostToDpuMachineIds[hostMachineID]
	out := make([]string, len(ids))
	copy(out, ids)
	return out, nil
}

func (c *mockClient) FindInstanceIdByMachineId(
	_ context.Context, machineID string,
) (string, error) {
	if machineID == "" {
		return "", errors.New("machine id is required")
	}
	return c.hostToInstanceID[machineID], nil
}

func (c *mockClient) InvokeInstancePower(
	_ context.Context, instanceID string, applyUpdates bool,
) error {
	if c.invokeInstancePowerErr != nil {
		return c.invokeInstancePowerErr
	}
	if instanceID == "" {
		return errors.New("instance id is required")
	}
	c.instancePowerCalls = append(c.instancePowerCalls, InstancePowerCall{
		InstanceID:   instanceID,
		ApplyUpdates: applyUpdates,
	})
	return nil
}

// SetHostDpuMachineIds wires a host machine to its DPU children for the
// FindAssociatedDpuMachineIds lookup (mock only).
func (c *mockClient) SetHostDpuMachineIds(hostMachineID string, dpuIDs []string) {
	out := make([]string, len(dpuIDs))
	copy(out, dpuIDs)
	c.hostToDpuMachineIds[hostMachineID] = out
}

// SetHostInstanceID configures the instance currently attached to the
// host. Pass "" to record "no instance attached" (mock only).
func (c *mockClient) SetHostInstanceID(hostMachineID string, instanceID string) {
	c.hostToInstanceID[hostMachineID] = instanceID
}

// SetDpuReprovisioningPending toggles whether
// IsDpuReprovisioningPendingForHost reports a host as still in progress
// (mock only). Tests use this to simulate "DPU disappeared from the
// pending list" between polls.
func (c *mockClient) SetDpuReprovisioningPending(hostMachineID string, pending bool) {
	c.pendingDpuReprovHosts[hostMachineID] = pending
}

// SetInsertHostUpdateOverrideError configures the error returned by
// InsertHostUpdateInProgressHealthOverride (mock only).
func (c *mockClient) SetInsertHostUpdateOverrideError(err error) {
	c.insertHostUpdateOverrideErr = err
}

// SetTriggerDpuReprovisioningError configures the error returned by
// TriggerDpuReprovisioning (mock only).
func (c *mockClient) SetTriggerDpuReprovisioningError(err error) {
	c.triggerDpuReprovisioningErr = err
}

// SetInvokeInstancePowerError configures the error returned by
// InvokeInstancePower (mock only).
func (c *mockClient) SetInvokeInstancePowerError(err error) {
	c.invokeInstancePowerErr = err
}

// DpuReprovisioningTriggers returns the recorded TriggerDpuReprovisioning
// calls in order (mock only). Returned slice is a defensive copy.
func (c *mockClient) DpuReprovisioningTriggers() []DpuReprovisioningCall {
	out := make([]DpuReprovisioningCall, len(c.dpuReprovisioningTriggers))
	copy(out, c.dpuReprovisioningTriggers)
	return out
}

// InstancePowerCalls returns the recorded InvokeInstancePower calls in
// order (mock only). Returned slice is a defensive copy.
func (c *mockClient) InstancePowerCalls() []InstancePowerCall {
	out := make([]InstancePowerCall, len(c.instancePowerCalls))
	copy(out, c.instancePowerCalls)
	return out
}

// HostUpdateOverridesActive returns the set of machine ids for which the
// HostUpdateInProgress override is currently inserted (mock only).
func (c *mockClient) HostUpdateOverridesActive() map[string]string {
	out := make(map[string]string, len(c.hostUpdateInProgressOverrides))
	for k, v := range c.hostUpdateInProgressOverrides {
		out[k] = v
	}
	return out
}

func (c *mockClient) FindMachineControllerStates(_ context.Context, machineIDs []string) (map[string]string, error) {
	if len(machineIDs) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(machineIDs))
	for _, id := range machineIDs {
		if s, ok := c.machineControllerStates[id]; ok && s != "" {
			out[id] = s
		}
	}
	return out, nil
}

func (c *mockClient) DecommissionMachine(_ context.Context, _ string) error {
	return c.decommissionMachineErr
}

func (c *mockClient) DecommissionSwitch(_ context.Context, _ string) error {
	return c.decommissionSwitchErr
}

func (c *mockClient) DecommissionPowerShelf(_ context.Context, _ string) error {
	return c.decommissionPowerShelfErr
}

// SetMachineControllerState records the raw state Core reports for a machine
// (mock only).
func (c *mockClient) SetMachineControllerState(machineID, state string) {
	c.machineControllerStates[machineID] = state
}

// SetDecommissionMachineError configures the error returned by DecommissionMachine
// (mock only).
func (c *mockClient) SetDecommissionMachineError(err error) {
	c.decommissionMachineErr = err
}

// SetDecommissionSwitchError configures the error returned by DecommissionSwitch
// (mock only).
func (c *mockClient) SetDecommissionSwitchError(err error) {
	c.decommissionSwitchErr = err
}

// SetDecommissionPowerShelfError configures the error returned by DecommissionPowerShelf
// (mock only).
func (c *mockClient) SetDecommissionPowerShelfError(err error) {
	c.decommissionPowerShelfErr = err
}
