// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"fmt"
	"strings"

	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type actualSyncResult struct {
	componentType devicetypes.ComponentType
	drifts        []model.ComponentDrift
	syncOK        bool
}

// runActualSync runs every per-type actual-vs-expected drift detector and
// returns each type's result independently. A type's result is authoritative
// only when syncOK is true. The observed-domain projection remains best effort
// because it does not contribute component drifts.
func runActualSync(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
) []actualSyncResult {
	results := make([]actualSyncResult, 0, 3)

	computeReceived, machineDrifts, machineOK := syncMachines(ctx, pool, nicoClient)
	results = append(results, actualSyncResult{
		componentType: devicetypes.ComponentTypeCompute,
		drifts:        machineDrifts,
		syncOK:        machineOK,
	})

	switchesReceived, nvSwitchDrifts, switchOK := syncNVSwitchesNICo(ctx, pool, nicoClient)
	results = append(results, actualSyncResult{
		componentType: devicetypes.ComponentTypeNVSwitch,
		drifts:        nvSwitchDrifts,
		syncOK:        switchOK,
	})

	// Domain membership is observed topology rather than expected inventory.
	// Project it after switch sync so this cycle's switch links are available.
	syncObservedNVLinkDomainTopology(ctx, pool, nicoClient)

	powershelvesReceived, powershelfDrifts, powershelfOK := syncPowershelvesNICo(ctx, pool, nicoClient)
	results = append(results, actualSyncResult{
		componentType: devicetypes.ComponentTypePowerShelf,
		drifts:        powershelfDrifts,
		syncOK:        powershelfOK,
	})

	allSyncOK := machineOK && switchOK && powershelfOK

	log.Info().
		Int("compute", computeReceived).
		Int("nvswitches", switchesReceived).
		Int("powershelves", powershelvesReceived).
		// Keep the established field for log-query compatibility. Its value has
		// always represented the safety of the complete drift replacement, which
		// now includes identity-reconciliation failures as well as RPC failures.
		Bool("all_rpc_ok", allSyncOK).
		Bool("all_sync_ok", allSyncOK).
		Msgf("Inventory received from Core: compute=%d nvswitches=%d powershelves=%d",
			computeReceived, switchesReceived, powershelvesReceived)

	return results
}

// mapKeys returns the keys of a string-keyed component map in arbitrary
// order. Used by the switch / power-shelf syncs to build the id slice they
// pass to the controller-state RPCs.
func mapKeys(m map[string]*model.Component) []string {
	if len(m) == 0 {
		return nil
	}
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	return out
}

// persistComponentOperationStatuses maps raw core controller_state strings to
// ComponentOperationStatus values via the per-type mapper and writes any deltas to the
// component table. components are keyed by external_id (machineID / switchID /
// shelfID). Entries without a state in statesByID are skipped — missing data
// is not a status reset.
func persistComponentOperationStatuses(
	ctx context.Context,
	pool *cdb.Session,
	componentType types.ComponentType,
	statesByID map[string]string,
	componentsByExternalID map[string]*model.Component,
) {
	if len(statesByID) == 0 {
		return
	}

	var toUpdate []model.Component
	for externalID, raw := range statesByID {
		comp, ok := componentsByExternalID[externalID]
		if !ok {
			continue
		}
		newStatus := nicoapi.MapComponentOperationStatus(componentType, raw)
		if comp.Status != nil && comp.Status.Equal(newStatus) {
			continue
		}
		comp.Status = &newStatus
		toUpdate = append(toUpdate, *comp)
	}

	if len(toUpdate) == 0 {
		return
	}
	if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		for _, cur := range toUpdate {
			if err := cur.SetStatusByComponentID(ctx, tx); err != nil {
				return fmt.Errorf("set component status: %w", err)
			}
		}
		return nil
	}); err != nil {
		log.Error().Msgf("Unable to persist component statuses: %v", err)
	}
}

// applyInventoryToComponents projects every runtime field used by switch and
// power-shelf synchronization.
func applyInventoryToComponents(
	ctx context.Context,
	pool *cdb.Session,
	resp *corev1.GetComponentInventoryResponse,
	componentsByID map[string]*model.Component,
) {
	firmwareUpdates := make([]model.Component, 0, len(resp.GetEntries()))
	powerStateUpdates := make([]model.Component, 0, len(resp.GetEntries()))
	for _, entry := range resp.GetEntries() {
		comp, report, ok := successfulInventoryEntry(entry, componentsByID)
		if !ok {
			continue
		}

		if version := bmcFirmwareVersion(report); version != "" && comp.FirmwareVersion != version {
			comp.FirmwareVersion = version
			firmwareUpdates = append(firmwareUpdates, *comp)
		}

		if systems := report.GetSystems(); len(systems) > 0 {
			ps := computerSystemPowerStateToNICo(systems[0].GetPowerState())
			if comp.PowerState == nil || *comp.PowerState != ps {
				comp.PowerState = &ps
				powerStateUpdates = append(powerStateUpdates, *comp)
			}
		}
	}

	if len(firmwareUpdates) == 0 && len(powerStateUpdates) == 0 {
		return
	}
	if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		for i := range firmwareUpdates {
			if err := firmwareUpdates[i].SetFirmwareVersionByComponentID(ctx, tx); err != nil {
				return fmt.Errorf("set firmware version: %w", err)
			}
		}
		for i := range powerStateUpdates {
			if err := powerStateUpdates[i].SetPowerStateByComponentID(ctx, tx); err != nil {
				return fmt.Errorf("set power state: %w", err)
			}
		}
		return nil
	}); err != nil {
		log.Error().Msgf("Unable to persist component inventory fields: %v", err)
	}
}

// applyFirmwareInventoryToComponents projects only firmware_version. Compute
// power state remains owned by GetPowerStates rather than the exploration
// report returned by GetComponentInventory. Use a targeted column update so a
// stale component snapshot cannot overwrite fields owned by another job.
func applyFirmwareInventoryToComponents(
	ctx context.Context,
	pool *cdb.Session,
	resp *corev1.GetComponentInventoryResponse,
	componentsByID map[string]*model.Component,
) {
	toUpdate := make([]model.Component, 0, len(resp.GetEntries()))
	for _, entry := range resp.GetEntries() {
		comp, report, ok := successfulInventoryEntry(entry, componentsByID)
		if !ok {
			continue
		}
		if version := bmcFirmwareVersion(report); version != "" && comp.FirmwareVersion != version {
			comp.FirmwareVersion = version
			toUpdate = append(toUpdate, *comp)
		}
	}

	if len(toUpdate) == 0 {
		return
	}
	if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		for i := range toUpdate {
			if err := toUpdate[i].SetFirmwareVersionByComponentID(ctx, tx); err != nil {
				return fmt.Errorf("set firmware version: %w", err)
			}
		}
		return nil
	}); err != nil {
		log.Error().Msgf("Unable to persist component firmware versions: %v", err)
	}
}

// successfulInventoryEntry validates an inventory entry and resolves the
// component_id echoed by Core to its Flow component.
func successfulInventoryEntry(
	entry *corev1.ComponentInventoryEntry,
	componentsByID map[string]*model.Component,
) (*model.Component, *corev1.EndpointExplorationReport, bool) {
	result := entry.GetResult()
	if result == nil {
		return nil, nil, false
	}
	comp, ok := componentsByID[result.GetComponentId()]
	if !ok {
		return nil, nil, false
	}
	if result.GetStatus() != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
		log.Warn().Msgf("Component %s: inventory status %s: %s", result.GetComponentId(), result.GetStatus(), result.GetError())
		return nil, nil, false
	}
	if entry.GetReport() == nil {
		return nil, nil, false
	}
	return comp, entry.GetReport(), true
}

// bmcFirmwareVersion resolves BMC firmware for compute, switch, and power-shelf
// inventory. Precedence is Core's canonical "bmc" version, then a known host
// BMC inventory ID, then the legacy "BMC image" description. Retaining the
// description match as a fallback lets a host BMC ID later in the report win
// over accelerator BMC entries regardless of inventory ordering.
func bmcFirmwareVersion(report *corev1.EndpointExplorationReport) string {
	if report == nil {
		return ""
	}
	if version := strings.TrimSpace(report.GetFirmwareVersions()["bmc"]); version != "" {
		return version
	}

	descriptionFallback := ""
	for _, svc := range report.GetService() {
		for _, inv := range svc.GetInventories() {
			version := strings.TrimSpace(inv.GetVersion())
			if version == "" {
				continue
			}
			switch inv.GetId() {
			case "BMC", "FW_BMC_0", "HostBMC_0":
				return version
			}
			if descriptionFallback == "" && inv.GetDescription() == "BMC image" {
				descriptionFallback = version
			}
		}
	}
	return descriptionFallback
}

func computerSystemPowerStateToNICo(
	ps corev1.ComputerSystemPowerState,
) nicoapi.PowerState {
	switch ps {
	case corev1.ComputerSystemPowerState_On, corev1.ComputerSystemPowerState_PoweringOn:
		return nicoapi.PowerStateOn
	case corev1.ComputerSystemPowerState_Off, corev1.ComputerSystemPowerState_PoweringOff:
		return nicoapi.PowerStateOff
	case corev1.ComputerSystemPowerState_Hibernating:
		return nicoapi.PowerStateHibernating
	case corev1.ComputerSystemPowerState_Sleeping:
		return nicoapi.PowerStateSleeping
	default:
		return nicoapi.PowerStateUnknown
	}
}
