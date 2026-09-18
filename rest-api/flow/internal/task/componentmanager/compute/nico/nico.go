// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package nico is the compute manager implementation that drives compute
// trays through NICo Core's Component Manager dispatch (the same RPCs
// already used by nvswitch/nico and powershelf/nico):
//
//   - PowerControl     -> ComponentPowerControl with MachineIds target
//   - GetPowerStatus   -> GetComponentInventory  with MachineIds target
//   - FirmwareControl  -> UpdateComponentFirmware with ComputeTrays target
//   - GetFirmwareStatus -> GetComponentFirmwareStatus with MachineIds target
//
// It replaces the legacy machine-centric path implemented in
// compute/nicolegacy. The legacy package routed power and firmware
// through machine-level RPCs (AdminPowerControl, UpdatePowerOption,
// SetMachineAutoUpdate, SetFirmwareUpdateTimeWindow). Those RPCs do not
// flow through Core's Component Manager / state controller pipeline,
// which is required to share orchestration logic across compute, nvswitch,
// and powershelf and to enable per-sub-target firmware updates.
//
// InjectExpectation, BringUpControl, and GetBringUpStatus do not have
// Component Manager equivalents in Core; they continue to call the
// existing machine-level RPCs that the legacy path used.
//
// The embedded service config selects this implementation by default.
// Operators can still opt back into compute/nicolegacy via
// COMPONENT_MANAGER_COMPUTE=nicolegacy. Core must be configured with
// compute_tray_use_state_controller=true (and SoT firmware objects) for
// this path to work.
package nico

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/common/dpureprov"
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

// ImplementationName is the name used to identify this implementation in
// the component manager catalog and in YAML / env configuration.
const ImplementationName = "nico"

// Manager manages compute trays via NICo Core's Component Manager RPCs.
type Manager struct {
	nicoClient nicoapi.Client
	// readiness guards mutating operations from running while any target
	// machine is reported as not ready for the operation by its persisted
	// ComponentOperationStatus. Identical safety contract to compute/nicolegacy:
	// the gate runs in Flow because Core's Component Manager dispatch
	// does not (yet) check host readiness state on its own.
	readiness readiness.Gate
	// dpuReprovOpts is the Options struct forwarded to dpureprov when
	// the DPU branch of FirmwareControl runs. Production leaves this
	// zero-valued so the dpureprov package's defaults (30s poll, 90min
	// timeout) apply; tests inject a tighter clock + interval via
	// the package-internal helper to keep poll loops virtual.
	dpuReprovOpts dpureprov.Options
}

// New creates a new compute Manager that drives Core's Component Manager
// dispatch. gate is used to gate disruptive operations on hosts that are
// not ready for them; a nil gate short-circuits to permissive in tests.
func New(nicoClient nicoapi.Client, gate readiness.Gate) *Manager {
	return &Manager{
		nicoClient: nicoClient,
		readiness:  gate,
	}
}

// Factory returns a factory that closes over the shared readiness gate.
// The gate is built once at service startup from the live DB session so
// every manager shares the same StatusReader.
func Factory(gate readiness.Gate) componentmanager.ManagerFactory {
	return func(
		providerRegistry *providerapi.ProviderRegistry,
	) (componentmanager.ComponentManager, error) {
		provider, err := providerapi.GetTyped[*nicoprovider.Provider](
			providerRegistry,
			nicoprovider.ProviderName,
		)
		if err != nil {
			return nil, fmt.Errorf("compute/nico requires nico provider: %w", err)
		}
		return New(provider.Client(), gate), nil
	}
}

// Descriptor returns the compute/nico manager descriptor.
func Descriptor() cmcatalog.Descriptor {
	return cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: ImplementationName,
		},
		RequiredProviders: []string{nicoprovider.ProviderName},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityBringUpControl,
			capability.CapabilityBringUpStatus,
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

// FactorySpec returns the compute/nico runtime factory spec.
func FactorySpec(gate readiness.Gate) componentmanager.FactorySpec {
	return componentmanager.FactorySpec{
		Descriptor: Descriptor(),
		Factory:    Factory(gate),
	}
}

// Descriptor returns the compute/nico manager descriptor.
func (m *Manager) Descriptor() cmcatalog.Descriptor {
	return Descriptor()
}

func machineIDsProto(ids []string) *corev1.MachineIdList {
	pbIDs := make([]*corev1.MachineId, len(ids))
	for i, id := range ids {
		pbIDs[i] = &corev1.MachineId{Id: id}
	}
	return &corev1.MachineIdList{MachineIds: pbIDs}
}

// ensureMachinesOperable is the per-Manager policy gate for disruptive
// operations on the given machines. The default policy refuses to proceed
// while any target host is reported as not ready for op by its persisted
// ComponentOperationStatus.
//
// When overrideReadinessCheck is true the gate is short-circuited and
// the operation runs unconditionally; the bypass is logged so it remains
// auditable from the worker log alone.
func (m *Manager) ensureMachinesOperable(
	ctx context.Context,
	machineIDs []string,
	op types.OperationType,
	overrideReadinessCheck bool,
) error {
	// A nil gate is the documented permissive mode: skip rather than
	// dispatch on a nil interface.
	if m.readiness == nil {
		return nil
	}
	if overrideReadinessCheck {
		log.Warn().
			Strs("machine_ids", machineIDs).
			Str("operation", string(op)).
			Msg("Readiness check bypassed by override_readiness_check on compute operation")
		return nil
	}
	return m.readiness.WaitForComponentsReady(ctx, machineIDs, op)
}

// InjectExpectation registers an expected machine with NICo via
// AddExpectedMachine. Core does not (yet) expose a Component Manager
// flavor of this RPC, so the call mirrors compute/nicolegacy.
func (m *Manager) InjectExpectation(
	ctx context.Context,
	target common.Target,
	info operations.InjectExpectationTaskInfo,
) error {
	var req nicoapi.AddExpectedMachineRequest
	if err := json.Unmarshal(info.Info, &req); err != nil {
		return fmt.Errorf("failed to unmarshal AddExpectedMachineRequest: %w", err)
	}

	if m.nicoClient == nil {
		return fmt.Errorf("nico client is not configured")
	}

	if err := m.nicoClient.AddExpectedMachine(ctx, req); err != nil {
		return fmt.Errorf("failed to add expected machine: %w", err)
	}

	log.Info().
		Str("bmc_mac", req.BMCMACAddress).
		Str("chassis_serial", req.ChassisSerialNumber).
		Msg("Successfully registered expected machine with NICo")

	return nil
}

// PowerControl performs power operations on compute trays via NICo's
// ComponentPowerControl RPC. Unlike compute/nicolegacy, this path does
// not insert per-machine health-report overrides or stagger calls: Core
// owns the per-component bookkeeping behind ComponentPowerControl.
func (m *Manager) PowerControl(
	ctx context.Context,
	target common.Target,
	info operations.PowerControlTaskInfo,
) error {
	log.Debug().Msgf(
		"compute power control %s op %s via NICo Component Manager",
		target.String(),
		info.Operation.String(),
	)

	if err := target.Validate(); err != nil {
		return fmt.Errorf("target is invalid: %w", err)
	}

	if err := m.ensureMachinesOperable(ctx, target.ComponentIDs, types.OperationTypePowerControl, info.OverrideReadinessCheck); err != nil {
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
	case operations.PowerOperationRestart, operations.PowerOperationWarmReset:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_GRACEFUL_RESTART
	case operations.PowerOperationForceRestart:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_FORCE_RESTART
	case operations.PowerOperationColdReset:
		action = corev1.SystemPowerControl_SYSTEM_POWER_CONTROL_AC_POWERCYCLE
	default:
		return fmt.Errorf("unsupported power operation for compute: %v", info.Operation)
	}

	req := &corev1.ComponentPowerControlRequest{
		Target: &corev1.ComponentPowerControlRequest_MachineIds{
			MachineIds: machineIDsProto(target.ComponentIDs),
		},
		Action:                action,
		BypassStateController: info.OverrideReadinessCheck,
	}

	resp, err := m.nicoClient.ComponentPowerControl(ctx, req)
	if err != nil {
		return fmt.Errorf("ComponentPowerControl failed: %w", err)
	}

	for _, r := range resp.GetResults() {
		if r.GetStatus() != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
			return fmt.Errorf("power control failed for %s: %s", r.GetComponentId(), r.GetError())
		}
	}

	log.Info().Msgf("compute power control %s on %s completed via NICo Component Manager",
		info.Operation.String(), target.String())
	return nil
}

// GetPowerStatus returns the power state for each compute tray by
// inspecting Core's per-component exploration reports.
func (m *Manager) GetPowerStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.PowerStatus, error) {
	if err := target.Validate(); err != nil {
		return nil, fmt.Errorf("target is invalid: %w", err)
	}

	req := &corev1.GetComponentInventoryRequest{
		Target: &corev1.GetComponentInventoryRequest_MachineIds{
			MachineIds: machineIDsProto(target.ComponentIDs),
		},
	}

	resp, err := m.nicoClient.GetComponentInventory(ctx, req)
	if err != nil {
		return nil, fmt.Errorf("GetComponentInventory failed: %w", err)
	}

	result := make(map[string]operations.PowerStatus, len(target.ComponentIDs))
	for _, id := range target.ComponentIDs {
		result[id] = operations.PowerStatusUnknown
	}

	for _, entry := range resp.GetEntries() {
		compID := entry.GetResult().GetComponentId()
		if ps := nicoprovider.ExtractPowerState(entry.GetReport()); ps != operations.PowerStatusUnknown {
			result[compID] = ps
		}
	}

	return result, nil
}

// FirmwareControl schedules a firmware update for compute trays via
// NICo's UpdateComponentFirmware RPC. Sub-targets are translated into
// the Core ComputeTrayComponent enum so the update can be scoped to
// e.g. just BMC or BIOS.
//
// info.TargetVersion is forwarded verbatim to UpdateComponentFirmware.
// Unlike compute/nicolegacy this is no longer a JSON object of component
// versions but the SoT firmware-object identifier interpreted by Core;
// Flow does not pre-validate it. info.StartTime / info.EndTime are not
// used by this path (Core does not expose a maintenance-window
// scheduling parameter on UpdateComponentFirmware) and are logged when
// set.
//
// # The "dpu" sub-target
//
// When info.SubTargets contains "dpu" the function ALSO runs the DPU
// reprovisioning sequence (see compute/common/dpureprov) against the targeted
// hosts. The two paths are sequenced rather than concurrent: any
// compute-tray-internal sub-targets (BMC / BIOS / NIC / etc.) are
// dispatched to UpdateComponentFirmware first, and DPU reprovisioning
// runs only after that call returns. This matches the operator runbook
// the manual `fac ytl` flow follows and keeps the host's BMC / BIOS at
// the desired version *before* the DPU reprov reboot kicks in.
//
// "dpu" is intentionally NOT included in the "empty SubTargets means
// update everything" default; the caller has to opt in explicitly.
// info.TargetVersion is ignored on the DPU branch (see dpureprov
// package doc for the rationale).
func (m *Manager) FirmwareControl(
	ctx context.Context,
	target common.Target,
	info operations.FirmwareControlTaskInfo,
) error {
	log.Debug().
		Str("components", target.String()).
		Str("target_version", info.TargetVersion).
		Strs("sub_targets", info.SubTargets).
		Msg("Starting firmware update for compute via NICo Component Manager")

	if err := target.Validate(); err != nil {
		return fmt.Errorf("target is invalid: %w", err)
	}

	if err := m.ensureMachinesOperable(ctx, target.ComponentIDs, types.OperationTypeFirmwareControl, info.OverrideReadinessCheck); err != nil {
		return fmt.Errorf("refused: %w", err)
	}

	if info.StartTime != 0 || info.EndTime != 0 {
		log.Warn().
			Int64("start_time", info.StartTime).
			Int64("end_time", info.EndTime).
			Msg("compute/nico ignores firmware update time window; Core schedules immediately via UpdateComponentFirmware")
	}

	computeTraySubs, hasDpu := firmwarecomponents.SplitNICoComputeTraySubTargets(info.SubTargets)

	// Run the compute-tray-internal update unless the request was
	// scoped to "dpu" only. We treat (info.SubTargets non-empty AND
	// computeTraySubs empty AND hasDpu) as "DPU-only" -- i.e. an opt-in
	// to the reprovisioning side-channel without touching tray
	// firmware. This explicit branch is needed because Core's
	// UpdateComponentFirmware with an empty Components list means
	// "update everything in the bundle", which is NOT what the caller
	// asked for in a DPU-only request.
	dpuOnly := firmwarecomponents.IsNICoDPUOnlySubTargets(info.SubTargets)
	if dpuOnly && info.AccessToken != "" {
		return fmt.Errorf(
			"dpu-only firmware updates do not support authentication data",
		)
	}
	if !dpuOnly {
		if err := m.firmwareControlComputeTrays(
			ctx,
			target,
			info.TargetVersion,
			computeTraySubs,
			info.AccessToken,
			info.OverrideVersionCheck,
			info.OverrideReadinessCheck,
		); err != nil {
			return err
		}
	}

	if hasDpu {
		if err := m.firmwareControlDpus(ctx, target); err != nil {
			return err
		}
	}

	log.Info().
		Str("components", target.String()).
		Str("target_version", info.TargetVersion).
		Bool("dpu_target", hasDpu).
		Msg("Firmware update completed for compute via NICo Component Manager")
	return nil
}

// firmwareControlComputeTrays dispatches the compute-tray-internal
// (BMC / BIOS / NIC / etc.) firmware update via Core's
// UpdateComponentFirmware. Split out from FirmwareControl so the DPU
// branch can skip it cleanly when the request is DPU-only.
func (m *Manager) firmwareControlComputeTrays(
	ctx context.Context,
	target common.Target,
	targetVersion string,
	computeTraySubs []string,
	accessToken string,
	overrideVersionCheck bool,
	bypassStateController bool,
) error {
	subComponents, err := firmwarecomponents.ParseNICoComputeTray(computeTraySubs)
	if err != nil {
		return err
	}

	req := &corev1.UpdateComponentFirmwareRequest{
		Target: &corev1.UpdateComponentFirmwareRequest_ComputeTrays{
			ComputeTrays: &corev1.UpdateComputeTrayFirmwareTarget{
				MachineIds: machineIDsProto(target.ComponentIDs),
				Components: subComponents,
			},
		},
		TargetVersion:         targetVersion,
		ForceUpdate:           overrideVersionCheck,
		BypassStateController: bypassStateController,
	}
	if accessToken != "" {
		req.AccessToken = &accessToken
	}

	resp, err := m.nicoClient.UpdateComponentFirmware(ctx, req)
	if err != nil {
		return fmt.Errorf("UpdateComponentFirmware failed: %w", err)
	}

	for _, r := range resp.GetResults() {
		if r.GetStatus() != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
			return fmt.Errorf("firmware update failed for %s: %s", r.GetComponentId(), r.GetError())
		}
	}
	return nil
}

// firmwareControlDpus runs DPU reprovisioning on every host listed in
// the target. Each host is reprovisioned serially via the four-step
// sequence implemented in compute/common/dpureprov. The per-request
// target version is not forwarded to Core: its reprovisioning state
// machine resolves the target firmware version from site configuration
// rather than per request — see the dpureprov package doc.
func (m *Manager) firmwareControlDpus(
	ctx context.Context,
	target common.Target,
) error {
	return dpureprov.ReprovisionHosts(
		ctx, m.nicoClient, target.ComponentIDs,
		true, // update_firmware: tenant-driven DPU reprov always rolls firmware
		m.dpuReprovOpts,
	)
}

// GetFirmwareStatus returns the current firmware update status for each
// compute tray. Core may report multiple sub-component updates (BMC,
// BIOS, CEC, NIC, CPLD_*, GPU, CX7, ...) for the same tray, so the
// per-tray result aggregates them: any failure -> Failed; all completed
// -> Completed; otherwise still in progress.
func (m *Manager) GetFirmwareStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.FirmwareUpdateStatus, error) {
	log.Debug().
		Str("components", target.String()).
		Msg("GetFirmwareStatus for compute via NICo Component Manager")

	if err := target.Validate(); err != nil {
		return nil, fmt.Errorf("target is invalid: %w", err)
	}

	req := &corev1.GetComponentFirmwareStatusRequest{
		Target: &corev1.GetComponentFirmwareStatusRequest_MachineIds{
			MachineIds: machineIDsProto(target.ComponentIDs),
		},
	}

	resp, err := m.nicoClient.GetComponentFirmwareStatus(ctx, req)
	if err != nil {
		return nil, fmt.Errorf("GetComponentFirmwareStatus failed: %w", err)
	}

	grouped := make(map[string][]*corev1.FirmwareUpdateStatus)
	for _, s := range resp.GetStatuses() {
		compID := s.GetResult().GetComponentId()
		grouped[compID] = append(grouped[compID], s)
	}

	result := make(map[string]operations.FirmwareUpdateStatus, len(target.ComponentIDs))
	for _, compID := range target.ComponentIDs {
		result[compID] = aggregateNICoStatuses(compID, grouped[compID])
	}

	return result, nil
}

// aggregateNICoStatuses collapses the per-sub-component statuses Core
// returns for a compute tray into a single FirmwareUpdateStatus. The
// rule mirrors the nvswitch/nico aggregator: any Failed -> Failed; all
// Completed -> Completed; any unknown sub-status -> Queued (still in
// progress).
//
// TODO: validate that Core returns one status per requested sub-target
// once the proto carries sub-component identification. Until then, a
// missing sub-component cannot be distinguished from "no update needed"
// and is treated as not-yet-completed.
func aggregateNICoStatuses(compID string, statuses []*corev1.FirmwareUpdateStatus) operations.FirmwareUpdateStatus {
	if len(statuses) == 0 {
		return operations.FirmwareUpdateStatus{
			ComponentID: compID,
			State:       operations.FirmwareUpdateStateUnknown,
		}
	}

	allCompleted := true
	var failures []string

	for _, s := range statuses {
		mapped := nicoprovider.MapFirmwareState(s.GetState())
		switch mapped {
		case operations.FirmwareUpdateStateFailed:
			errMsg := s.GetResult().GetError()
			if errMsg == "" {
				errMsg = s.GetState().String()
			}
			failures = append(failures, errMsg)
		case operations.FirmwareUpdateStateCompleted:
			// ok
		default:
			allCompleted = false
		}
	}

	if len(failures) > 0 {
		return operations.FirmwareUpdateStatus{
			ComponentID: compID,
			State:       operations.FirmwareUpdateStateFailed,
			Error:       fmt.Sprintf("firmware update failed for components: %s", strings.Join(failures, "; ")),
		}
	}

	if allCompleted {
		return operations.FirmwareUpdateStatus{
			ComponentID: compID,
			State:       operations.FirmwareUpdateStateCompleted,
		}
	}

	return operations.FirmwareUpdateStatus{
		ComponentID: compID,
		State:       operations.FirmwareUpdateStateQueued,
	}
}

// BringUpControl opens the NICo power-on gate for each compute component.
// Core does not expose a Component Manager flavor of this operation, so
// the call mirrors compute/nicolegacy.
func (m *Manager) BringUpControl(
	ctx context.Context,
	target common.Target,
	info operations.BringUpTaskInfo,
) error {
	log.Debug().
		Str("components", target.String()).
		Msg("BringUpControl for compute (legacy NICo RPC)")

	if m.nicoClient == nil {
		return fmt.Errorf("nico client is not configured")
	}

	if err := target.Validate(); err != nil {
		return fmt.Errorf("target is invalid: %w", err)
	}

	// BringUpControl can trigger a power-on, so we gate on the same
	// readiness signal that PowerControl would consult.
	if err := m.ensureMachinesOperable(ctx, target.ComponentIDs, types.OperationTypePowerControl, info.OverrideReadinessCheck); err != nil {
		return fmt.Errorf("refused: %w", err)
	}

	for _, componentID := range target.ComponentIDs {
		if err := m.nicoClient.AllowIngestionAndPowerOn(ctx, componentID, ""); err != nil {
			return fmt.Errorf("BringUpControl failed for %s: %w", componentID, err)
		}
		log.Info().
			Str("component_id", componentID).
			Msg("BringUpControl succeeded")
	}

	return nil
}

// GetBringUpStatus returns the bring-up state for each compute component.
// Core does not expose a Component Manager flavor of this operation, so
// the call mirrors compute/nicolegacy.
func (m *Manager) GetBringUpStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.MachineBringUpState, error) {
	log.Debug().
		Str("components", target.String()).
		Msg("GetBringUpStatus for compute (legacy NICo RPC)")

	if m.nicoClient == nil {
		return nil, fmt.Errorf("nico client is not configured")
	}

	if err := target.Validate(); err != nil {
		return nil, fmt.Errorf("target is invalid: %w", err)
	}

	result := make(map[string]operations.MachineBringUpState, len(target.ComponentIDs))
	for _, componentID := range target.ComponentIDs {
		state, err := m.nicoClient.DetermineMachineIngestionState(ctx, componentID, "")
		if err != nil {
			return nil, fmt.Errorf("GetBringUpStatus failed for %s: %w", componentID, err)
		}
		result[componentID] = nicoToBringUpState(state)
	}

	return result, nil
}

func nicoToBringUpState(s nicoapi.BringUpState) operations.MachineBringUpState {
	switch s {
	case nicoapi.BringUpStateWaitingForIngestion:
		return operations.MachineBringUpStateWaitingForIngestion
	case nicoapi.BringUpStateMachineNotCreated:
		return operations.MachineBringUpStateMachineNotCreated
	case nicoapi.BringUpStateMachineCreated:
		return operations.MachineBringUpStateMachineCreated
	default:
		return operations.MachineBringUpStateNotDiscovered
	}
}

// Decommission initiates decommissioning of the target compute machines via NICo.
func (m *Manager) Decommission(
	ctx context.Context,
	target common.Target,
	_ operations.DecommissionTaskInfo,
) error {
	if err := target.Validate(); err != nil {
		return fmt.Errorf("target is invalid: %w", err)
	}

	for _, machineID := range target.ComponentIDs {
		if err := m.nicoClient.DecommissionMachine(ctx, machineID); err != nil {
			return fmt.Errorf("DecommissionMachine failed for %s: %w", machineID, err)
		}
	}

	log.Info().
		Strs("machine_ids", target.ComponentIDs).
		Msg("Decommission initiated for compute components")
	return nil
}

// GetDecommissionStatus returns the current decommission state for each
// target compute machine, keyed by machine ID.
func (m *Manager) GetDecommissionStatus(
	ctx context.Context,
	target common.Target,
) (map[string]string, error) {
	if err := target.Validate(); err != nil {
		return nil, fmt.Errorf("target is invalid: %w", err)
	}

	states, err := m.nicoClient.FindMachineControllerStates(ctx, target.ComponentIDs)
	if err != nil {
		return nil, fmt.Errorf("FindMachineControllerStates: %w", err)
	}

	// Ensure every requested component is present in the result.
	result := make(map[string]string, len(target.ComponentIDs))
	for _, id := range target.ComponentIDs {
		if s, ok := states[id]; ok {
			result[id] = normalizeDecommissionState(s)
		} else {
			result[id] = ""
		}
	}
	return result, nil
}

// normalizeDecommissionState converts Core's persisted managed-host JSON
// into the status vocabulary used by the Flow decommission waiter.
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
