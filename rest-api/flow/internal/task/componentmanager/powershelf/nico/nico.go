// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/firmwarecomponents"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	ImplementationName = "nico"
)

// Manager manages power shelf components via the NICo/NICo component dispatch RPCs.
type Manager struct {
	nicoClient nicoapi.Client
	// readiness guards power/firmware operations on a shelf from running
	// while any host on the shelf's rack is reported as not ready for
	// the operation by its persisted ComponentOperationStatus. PowerShelves feed
	// the entire rack, so toggling one can power-cycle every host
	// downstream of it; the check is therefore rack-scoped.
	readiness readiness.Gate
}

// New creates a new NICo-based PowerShelf Manager. A nil gate
// short-circuits to permissive in tests; production callers wire the
// shared DB-backed gate.
func New(nicoClient nicoapi.Client, gate readiness.Gate) *Manager {
	return &Manager{
		nicoClient: nicoClient,
		readiness:  gate,
	}
}

// Factory returns a factory that closes over the shared readiness gate.
// The factory retrieves the NICo provider from the registry and pairs it
// with the gate.
func Factory(gate readiness.Gate) componentmanager.ManagerFactory {
	return func(
		providerRegistry *providerapi.ProviderRegistry,
	) (componentmanager.ComponentManager, error) {
		provider, err := providerapi.GetTyped[*nicoprovider.Provider](
			providerRegistry,
			nicoprovider.ProviderName,
		)
		if err != nil {
			return nil, fmt.Errorf("powershelf/nico requires nico provider: %w", err)
		}
		return New(provider.Client(), gate), nil
	}
}

// Descriptor returns the NICo PowerShelf manager descriptor.
func Descriptor() cmcatalog.Descriptor {
	return cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypePowerShelf,
			Implementation: ImplementationName,
		},
		RequiredProviders: []string{nicoprovider.ProviderName},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityDecommissionControl,
			capability.CapabilityDecommissionStatus,
			capability.CapabilityFirmwareControl,
			capability.CapabilityFirmwareStatus,
			capability.CapabilityInjectExpectation,
			capability.CapabilityPowerControl,
			capability.CapabilityPowerStatus,
		},
	}
}

// FactorySpec returns the NICo PowerShelf manager runtime factory spec.
func FactorySpec(gate readiness.Gate) componentmanager.FactorySpec {
	return componentmanager.FactorySpec{
		Descriptor: Descriptor(),
		Factory:    Factory(gate),
	}
}

// Descriptor returns the NICo PowerShelf manager descriptor.
func (m *Manager) Descriptor() cmcatalog.Descriptor {
	return Descriptor()
}

func powerShelfIDsProto(ids []string) *corev1.PowerShelfIdList {
	pbIDs := make([]*corev1.PowerShelfId, len(ids))
	for i, id := range ids {
		pbIDs[i] = &corev1.PowerShelfId{Id: id}
	}
	return &corev1.PowerShelfIdList{Ids: pbIDs}
}

// ensureRackOperable is the per-Manager policy gate for disruptive
// operations on the racks that own the given power shelves. The default
// policy refuses to proceed while any host on the resolved rack(s) is
// reported as not ready for the operation by its persisted
// ComponentOperationStatus, because a shelf reset power-cycles every host
// downstream of it.
//
// When overrideReadinessCheck is true the gate is short-circuited
// without performing the rack lookup. The override is intended for
// operator-supervised maintenance windows; authorisation is enforced
// upstream and is not re-checked here. A warning is emitted so the
// bypass is auditable from the worker log alone.
//
// Shelves not associated with a rack in Core are skipped with a warning
// (see the equivalent NVSwitch helper for the reasoning).
// ensureTargetOperable keeps readiness resolution separate from API targeting.
func (m *Manager) ensureTargetOperable(ctx context.Context, target common.Target, op types.OperationType, override bool) error {
	if !target.UsesMACAddresses() {
		return m.ensureRackOperable(ctx, target.Identifiers, op, override)
	}
	if m.readiness == nil {
		return nil
	}
	if override {
		log.Warn().Strs("management_macs", target.Identifiers).Str("operation", string(op)).Msg("Readiness check bypassed by override_readiness_check")
		return nil
	}
	return m.readiness.WaitForManagementMACsReady(ctx, target.Type, target.Identifiers, op)
}

func (m *Manager) ensureRackOperable(
	ctx context.Context,
	shelfIDs []string,
	op types.OperationType,
	overrideReadinessCheck bool,
) error {
	if len(shelfIDs) == 0 {
		return nil
	}

	// A nil gate is the documented permissive mode: skip the rack lookup
	// entirely rather than dispatch on a nil interface.
	if m.readiness == nil {
		return nil
	}

	if overrideReadinessCheck {
		log.Warn().
			Strs("power_shelf_ids", shelfIDs).
			Str("operation", string(op)).
			Msg("Readiness check bypassed by override_readiness_check on PowerShelf operation")
		return nil
	}

	rackByShelf, err := m.nicoClient.FindPowerShelfRackIDs(ctx, shelfIDs)
	if err != nil {
		return fmt.Errorf("look up rack for power shelves: %w", err)
	}

	rackIDs := make([]string, 0, len(rackByShelf))
	for _, rid := range rackByShelf {
		rackIDs = append(rackIDs, rid)
	}

	var orphan []string
	for _, sid := range shelfIDs {
		if _, ok := rackByShelf[sid]; !ok {
			orphan = append(orphan, sid)
		}
	}
	if len(orphan) > 0 {
		log.Warn().
			Strs("power_shelf_ids", orphan).
			Msg("PowerShelf has no rack assignment; readiness check cannot be applied")
	}

	return m.readiness.WaitForRackHostsReady(ctx, rackIDs, op)
}

// InjectExpectation registers an expected power shelf with NICo via AddExpectedPowerShelf.
// The Info field should contain a JSON-encoded nicoapi.AddExpectedPowerShelfRequest.
func (m *Manager) InjectExpectation(
	ctx context.Context,
	target common.Target,
	info operations.InjectExpectationTaskInfo,
) error {
	if m.nicoClient == nil {
		return fmt.Errorf("nico client is not configured")
	}

	var req nicoapi.AddExpectedPowerShelfRequest
	if err := json.Unmarshal(info.Info, &req); err != nil {
		return fmt.Errorf("failed to unmarshal AddExpectedPowerShelfRequest: %w", err)
	}

	if err := m.nicoClient.AddExpectedPowerShelf(ctx, req); err != nil {
		return fmt.Errorf("failed to add expected power shelf: %w", err)
	}

	log.Info().
		Str("bmc_mac", req.BMCMACAddress).
		Str("shelf_serial", req.ShelfSerialNumber).
		Msg("Successfully registered expected power shelf with NICo")

	return nil
}

func (m *Manager) PowerControl(
	ctx context.Context,
	target common.Target,
	info operations.PowerControlTaskInfo,
) error {
	log.Debug().Msgf("PowerShelf power control %s op %s via NICo",
		target.String(), info.Operation.String())

	if err := target.Validate(); err != nil {
		return fmt.Errorf("target is invalid: %w", err)
	}

	if err := m.ensureTargetOperable(ctx, target, types.OperationTypePowerControl, info.OverrideReadinessCheck); err != nil {
		return fmt.Errorf("refused: %w", err)
	}

	var action corev1.SystemPowerControl
	switch info.Operation {
	case operations.PowerOperationPowerOn, operations.PowerOperationForcePowerOn:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_ON
	case operations.PowerOperationPowerOff:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_GRACEFUL_SHUTDOWN
	case operations.PowerOperationForcePowerOff:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_FORCE_OFF
	case operations.PowerOperationRestart:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_GRACEFUL_RESTART
	case operations.PowerOperationForceRestart:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_FORCE_RESTART
	default:
		return fmt.Errorf("unsupported power operation for PowerShelf: %v", info.Operation)
	}

	req := &corev1.ComponentPowerControlRequest{
		Action:                action,
		BypassStateController: info.OverrideReadinessCheck,
	}
	if target.UsesMACAddresses() {
		req.Target = &corev1.ComponentPowerControlRequest_PowerShelfPmcMacs{
			PowerShelfPmcMacs: &corev1.MacAddressList{MacAddresses: target.Identifiers},
		}
	} else {
		req.Target = &corev1.ComponentPowerControlRequest_PowerShelfIds{
			PowerShelfIds: powerShelfIDsProto(target.Identifiers),
		}
	}

	resp, err := m.nicoClient.ComponentPowerControl(ctx, req)
	if err != nil {
		return fmt.Errorf("ComponentPowerControl failed: %w", err)
	}

	for _, r := range resp.GetResults() {
		if r.GetStatus() != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
			return fmt.Errorf("power control failed for %s: %s", nicoprovider.ResultIdentifier(r, target.UsesMACAddresses()), r.GetError())
		}
	}

	log.Info().Msgf("PowerShelf power control %s on %s completed via NICo",
		info.Operation.String(), target.String())
	return nil
}

func (m *Manager) GetPowerStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.PowerStatus, error) {
	if err := target.Validate(); err != nil {
		return nil, fmt.Errorf("target is invalid: %w", err)
	}

	req := &corev1.GetComponentInventoryRequest{}
	if target.UsesMACAddresses() {
		req.Target = &corev1.GetComponentInventoryRequest_PowerShelfPmcMacs{
			PowerShelfPmcMacs: &corev1.MacAddressList{MacAddresses: target.Identifiers},
		}
	} else {
		req.Target = &corev1.GetComponentInventoryRequest_PowerShelfIds{
			PowerShelfIds: powerShelfIDsProto(target.Identifiers),
		}
	}

	resp, err := m.nicoClient.GetComponentInventory(ctx, req)
	if err != nil {
		return nil, fmt.Errorf("GetComponentInventory failed: %w", err)
	}

	result := make(map[string]operations.PowerStatus, target.Len())
	for _, entry := range resp.GetEntries() {
		if entry.GetResult() == nil || entry.GetResult().GetStatus() != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
			continue
		}
		compID := nicoprovider.ResultIdentifier(entry.GetResult(), target.UsesMACAddresses())
		result[compID] = nicoprovider.ExtractPowerState(entry.GetReport())
	}

	return result, nil
}

func (m *Manager) FirmwareControl(
	ctx context.Context,
	target common.Target,
	info operations.FirmwareControlTaskInfo,
) error {
	log.Debug().
		Str("components", target.String()).
		Str("target_version", info.TargetVersion).
		Strs("sub_targets", info.SubTargets).
		Msg("Starting firmware update for PowerShelf via NICo")

	if err := target.Validate(); err != nil {
		return fmt.Errorf("target is invalid: %w", err)
	}

	if err := m.ensureTargetOperable(ctx, target, types.OperationTypeFirmwareControl, info.OverrideReadinessCheck); err != nil {
		return fmt.Errorf("refused: %w", err)
	}

	subComponents, err := firmwarecomponents.ParseNICoPowerShelf(info.SubTargets)
	if err != nil {
		return err
	}
	if len(subComponents) == 0 {
		// Preserve historical behavior: when the caller does not specify a
		// subset, only PMC is updated. Once the component manager supports
		// "update everything in the bundle" semantics we can drop this.
		subComponents = []corev1.PowerShelfComponent{corev1.PowerShelfComponent_POWER_SHELF_COMPONENT_PMC}
	}

	powerShelfTarget := &corev1.UpdatePowerShelfFirmwareTarget{Components: subComponents}
	if target.UsesMACAddresses() {
		powerShelfTarget.PmcMacs = &corev1.MacAddressList{MacAddresses: target.Identifiers}
	} else {
		powerShelfTarget.PowerShelfIds = powerShelfIDsProto(target.Identifiers)
	}
	req := &corev1.UpdateComponentFirmwareRequest{
		Target: &corev1.UpdateComponentFirmwareRequest_PowerShelves{
			PowerShelves: powerShelfTarget,
		},
		TargetVersion:         info.TargetVersion,
		ForceUpdate:           info.OverrideVersionCheck,
		BypassStateController: info.OverrideReadinessCheck,
	}
	if info.AccessToken != "" {
		req.AccessToken = &info.AccessToken
	}

	resp, err := m.nicoClient.UpdateComponentFirmware(ctx, req)
	if err != nil {
		return fmt.Errorf("UpdateComponentFirmware failed: %w", err)
	}

	for _, r := range resp.GetResults() {
		if r.GetStatus() != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
			return fmt.Errorf("firmware update failed for %s: %s", nicoprovider.ResultIdentifier(r, target.UsesMACAddresses()), r.GetError())
		}
	}

	log.Info().
		Str("components", target.String()).
		Str("target_version", info.TargetVersion).
		Msg("Firmware update started for PowerShelf via NICo")
	return nil
}

func (m *Manager) GetFirmwareStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.FirmwareUpdateStatus, error) {
	log.Debug().
		Str("components", target.String()).
		Msg("GetFirmwareStatus for PowerShelf via NICo")

	if err := target.Validate(); err != nil {
		return nil, fmt.Errorf("target is invalid: %w", err)
	}

	req := &corev1.GetComponentFirmwareStatusRequest{}
	if target.UsesMACAddresses() {
		req.Target = &corev1.GetComponentFirmwareStatusRequest_PowerShelfPmcMacs{
			PowerShelfPmcMacs: &corev1.MacAddressList{MacAddresses: target.Identifiers},
		}
	} else {
		req.Target = &corev1.GetComponentFirmwareStatusRequest_PowerShelfIds{
			PowerShelfIds: powerShelfIDsProto(target.Identifiers),
		}
	}

	resp, err := m.nicoClient.GetComponentFirmwareStatus(ctx, req)
	if err != nil {
		return nil, fmt.Errorf("GetComponentFirmwareStatus failed: %w", err)
	}

	result := make(map[string]operations.FirmwareUpdateStatus, len(resp.GetStatuses()))
	for _, s := range resp.GetStatuses() {
		compID := nicoprovider.ResultIdentifier(s.GetResult(), target.UsesMACAddresses())
		result[compID] = operations.FirmwareUpdateStatus{
			ComponentID: compID,
			State:       nicoprovider.MapFirmwareState(s.GetState()),
		}
	}

	return result, nil
}

// Decommission initiates decommissioning of the target power shelves via NICo.
func (m *Manager) Decommission(
	ctx context.Context,
	target common.Target,
	_ operations.DecommissionTaskInfo,
) error {
	if err := target.Validate(); err != nil {
		return fmt.Errorf("target is invalid: %w", err)
	}

	for _, shelfID := range target.Identifiers {
		if err := m.nicoClient.DecommissionPowerShelf(ctx, shelfID); err != nil {
			return fmt.Errorf("DecommissionPowerShelf failed for %s: %w", shelfID, err)
		}
	}

	log.Info().
		Strs("shelf_ids", target.Identifiers).
		Msg("Decommission initiated for PowerShelf components")
	return nil
}

// GetDecommissionStatus returns the current decommission state for each
// target power shelf, keyed by shelf ID.
func (m *Manager) GetDecommissionStatus(
	ctx context.Context,
	target common.Target,
) (map[string]string, error) {
	if err := target.Validate(); err != nil {
		return nil, fmt.Errorf("target is invalid: %w", err)
	}

	states, err := m.nicoClient.FindPowerShelfControllerStates(ctx, target.Identifiers)
	if err != nil {
		return nil, fmt.Errorf("FindPowerShelfControllerStates: %w", err)
	}

	// Ensure every requested component is present in the result.
	result := make(map[string]string, len(target.Identifiers))
	for _, id := range target.Identifiers {
		if s, ok := states[id]; ok {
			result[id] = normalizeDecommissionState(s)
		} else {
			result[id] = ""
		}
	}
	return result, nil
}

// normalizeDecommissionState converts Core's persisted power-shelf-controller
// JSON into the status vocabulary used by the Flow decommission waiter.
func normalizeDecommissionState(raw string) string {
	var state struct {
		State                string `json:"state"`
		DecommissioningState struct {
			State string `json:"state"`
		} `json:"decommissioning_state"`
	}
	if err := json.Unmarshal([]byte(raw), &state); err != nil ||
		state.State != "decommissioning" ||
		state.DecommissioningState.State == "" {
		return raw
	}
	if state.DecommissioningState.State == "decommissioned" {
		return "Decommissioned"
	}
	return "Decommissioning/" + state.DecommissioningState.State
}
