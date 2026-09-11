// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"fmt"

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

// applyInventoryToComponents extracts firmware_version and power_state from
// GetComponentInventoryResponse and direct-writes them to the matching
// components. It does not emit drift: correlation and drift are keyed on BMC
// MAC (handled by each type's linked-RPC sync), and serial number is no longer
// compared. componentsByID maps the component_id echoed back in each
// ComponentResult to the DB component. Shared by the switch and power-shelf
// syncs; the machine sync uses pre-fetched MachineDetail directly instead of
// going through GetComponentInventory.
func applyInventoryToComponents(
	ctx context.Context,
	pool *cdb.Session,
	resp *corev1.GetComponentInventoryResponse,
	componentsByID map[string]*model.Component,
) {
	for _, entry := range resp.GetEntries() {
		result := entry.GetResult()
		if result == nil {
			continue
		}
		comp, ok := componentsByID[result.GetComponentId()]
		if !ok {
			continue
		}
		if result.GetStatus() != corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS {
			log.Warn().Msgf("Component %s: inventory status %s: %s", result.GetComponentId(), result.GetStatus(), result.GetError())
			continue
		}

		report := entry.GetReport()
		if report == nil {
			continue
		}

		needsUpdate := false

		// Extract firmware_version from the "BMC image" inventory entry
		for _, svc := range report.GetService() {
			for _, inv := range svc.GetInventories() {
				if inv.GetDescription() == "BMC image" {
					if v := inv.GetVersion(); v != "" && comp.FirmwareVersion != v {
						comp.FirmwareVersion = v
						needsUpdate = true
					}
				}
			}
		}

		// Extract power_state from first ComputerSystem entry
		if systems := report.GetSystems(); len(systems) > 0 {
			ps := computerSystemPowerStateToNICo(systems[0].GetPowerState())
			if comp.PowerState == nil || *comp.PowerState != ps {
				comp.PowerState = &ps
				needsUpdate = true
			}
		}

		if needsUpdate {
			if err := comp.Patch(ctx, pool.DB); err != nil {
				log.Error().Msgf("Component %s: unable to write inventory fields: %v", result.GetComponentId(), err)
			}
		}
	}
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
