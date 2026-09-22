// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"

	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// nvosIPDescriptionKey is the component.description key under which the
// switch's resolved NVOS host IP is recorded. Core owns the resolution; Flow
// only mirrors it so the IP is queryable alongside the component.
const nvosIPDescriptionKey = "nvos_ip"

// ---------------------------------------------------------------------------
// syncNVSwitchesNICo: sync NVSwitch components via Core (NICo)
// ---------------------------------------------------------------------------
//
// Uses Core's NICo API. Core's NSM backend auto-registers switches, so no
// registration step is needed.
//
// Primary NICo API calls:
//   - GetSwitches: list every active Core switch and its BMC MAC
//   - GetComponentInventory: get firmware, serial, power state from site explorer
//
// Flow:
//  1. DB: get all NVSwitch components with BMCs
//  2. NICo GetSwitches: map BMC MAC → Core SwitchId
//  3. Transactionally converge external_id from this snapshot
//  4. NICo GetComponentInventory: extract firmware_version, serial_number, power_state
//  5. Direct-write inventory fields to DB
//  6. Return current-snapshot drifts in both directions
func syncNVSwitchesNICo(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
) (received int, drifts []model.ComponentDrift, rpcOK bool) {
	log.Debug().Msg("Syncing NV switches via NICo...")

	expectedSwitches, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeNVSwitch)
	if err != nil {
		log.Error().Msgf("Unable to retrieve NVSwitch components from db: %v", err)
		return 0, nil, false
	}

	// Actual snapshot: map BMC MAC → Core SwitchId.
	observed, err := nicoClient.GetSwitches(ctx)
	if err != nil {
		log.Error().Msgf("Unable to retrieve active switches from NICo: %v", err)
		return 0, nil, false
	}
	linkedActual := make([]actualControllerDevice, 0, len(observed))
	for _, sw := range observed {
		linkedActual = append(linkedActual, actualControllerDevice{
			controllerMAC: sw.BmcMac,
			externalID:    sw.ID,
		})
	}
	received, componentsBySwitchID, drifts, reconcileOK := reconcileActualControllerDevices(
		ctx, pool, "NVSwitch", expectedSwitches, linkedActual,
	)
	if !reconcileOK {
		return received, nil, false
	}

	switchIDs := make([]*corev1.SwitchId, 0, len(componentsBySwitchID))
	for switchID := range componentsBySwitchID {
		switchIDs = append(switchIDs, &corev1.SwitchId{Id: switchID})
	}

	// Fetch inventory from Core for all matched switches. Inventory only feeds
	// the firmware_version / power_state direct-writes now — drift is keyed on
	// BMC MAC via the linked RPC above — so a failure here is best-effort and
	// just leaves those fields stale this cycle; it does not make the drift
	// table partial.
	if len(switchIDs) > 0 {
		invResp, err := nicoClient.GetComponentInventory(ctx, &corev1.GetComponentInventoryRequest{
			Target: &corev1.GetComponentInventoryRequest_SwitchIds{
				SwitchIds: &corev1.SwitchIdList{Ids: switchIDs},
			},
		})
		if err != nil {
			log.Error().Msgf("Unable to retrieve switch inventory from NICo: %v", err)
		} else {
			applyInventoryToComponents(ctx, pool, invResp, componentsBySwitchID)
		}
	}

	syncSwitchStatuses(ctx, pool, nicoClient, componentsBySwitchID)

	syncSwitchNvosIPs(ctx, pool, nicoClient, componentsBySwitchID)

	log.Info().Msgf("NVSwitch NICo sync: %d drift(s) out of %d expected", len(drifts), len(expectedSwitches))
	return received, drifts, true
}

// syncSwitchStatuses fetches controller_state for the matched switches and
// persists the derived ComponentOperationStatus per DB row.
func syncSwitchStatuses(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
	componentsBySwitchID map[string]*model.Component,
) {
	ids := mapKeys(componentsBySwitchID)
	if len(ids) == 0 {
		return
	}
	statesByID, err := nicoClient.FindSwitchControllerStates(ctx, ids)
	if err != nil {
		log.Error().Msgf("Unable to retrieve switch controller_states from NICo: %v", err)
		return
	}
	persistComponentOperationStatuses(ctx, pool, types.ComponentTypeNVSwitch, statesByID, componentsBySwitchID)
}

// syncSwitchNvosIPs records Core's resolved NVOS host IP for each matched
// switch in the component's description. Core only reports an NVOS IP once both
// the NVOS MAC and its assigned address resolve, so switches without one are
// left untouched rather than having the key cleared. The description merge
// preserves any other keys (operator-managed metadata, etc.).
func syncSwitchNvosIPs(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
	componentsBySwitchID map[string]*model.Component,
) {
	ids := mapKeys(componentsBySwitchID)
	if len(ids) == 0 {
		return
	}
	ipsByID, err := nicoClient.FindSwitchNvosIPs(ctx, ids)
	if err != nil {
		log.Error().Msgf("Unable to retrieve switch NVOS IPs from NICo: %v", err)
		return
	}
	for switchID, ip := range ipsByID {
		comp, ok := componentsBySwitchID[switchID]
		if !ok || ip == "" {
			continue
		}
		if existing, ok := comp.Description[nvosIPDescriptionKey].(string); ok && existing == ip {
			continue
		}
		if comp.Description == nil {
			comp.Description = map[string]any{}
		}
		comp.Description[nvosIPDescriptionKey] = ip
		if err := comp.Patch(ctx, pool.DB); err != nil {
			log.Error().Msgf("NVSwitch %s: unable to persist NVOS IP %s: %v", comp.ID, ip, err)
			continue
		}
		log.Info().Msgf("NVSwitch %s: recorded NVOS IP %s", comp.ID, ip)
	}
}
