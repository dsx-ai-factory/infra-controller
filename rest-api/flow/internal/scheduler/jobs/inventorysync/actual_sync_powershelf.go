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

// ---------------------------------------------------------------------------
// syncPowershelvesNICo: sync PowerShelf components via Core (NICo)
// ---------------------------------------------------------------------------
//
// Uses Core's NICo API. Core's PSM backend auto-registers power shelves, so no
// registration step is needed.
//
// Primary NICo API calls:
//   - GetPowerShelves: list every active Core power shelf and its BMC MAC
//   - GetComponentInventory: get firmware, power state from site explorer
//
// Flow:
//  1. DB: get all PowerShelf components with PMCs
//  2. NICo GetPowerShelves: map PMC MAC → Core PowerShelfId
//  3. Transactionally converge external_id from this snapshot
//  4. NICo GetComponentInventory: extract firmware_version, power_state
//  5. Direct-write inventory fields to DB
//  6. Return current-snapshot drifts in both directions
func syncPowershelvesNICo(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
) (received int, drifts []model.ComponentDrift, rpcOK bool) {
	log.Debug().Msg("Syncing powershelves via NICo...")

	expectedPowershelves, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypePowerShelf)
	if err != nil {
		log.Error().Msgf("Unable to retrieve powershelf components from db: %v", err)
		return 0, nil, false
	}

	// Actual snapshot: map PMC MAC → Core PowerShelfId.
	observed, err := nicoClient.GetPowerShelves(ctx)
	if err != nil {
		log.Error().Msgf("Unable to retrieve active power shelves from NICo: %v", err)
		return 0, nil, false
	}
	linkedActual := make([]actualControllerDevice, 0, len(observed))
	for _, shelf := range observed {
		linkedActual = append(linkedActual, actualControllerDevice{
			controllerMAC: shelf.BmcMac,
			externalID:    shelf.ID,
		})
	}
	received, componentsByShelfID, drifts, reconcileOK := reconcileActualControllerDevices(
		ctx, pool, "PowerShelf", expectedPowershelves, linkedActual,
	)
	if !reconcileOK {
		return received, nil, false
	}

	shelfIDs := make([]*corev1.PowerShelfId, 0, len(componentsByShelfID))
	for shelfID := range componentsByShelfID {
		shelfIDs = append(shelfIDs, &corev1.PowerShelfId{Id: shelfID})
	}

	// Fetch inventory from Core for all matched power shelves. Inventory only
	// feeds the firmware_version / power_state direct-writes now — drift is
	// keyed on PMC MAC via the linked RPC above — so a failure here is
	// best-effort and just leaves those fields stale this cycle; it does not
	// make the drift table partial.
	if len(shelfIDs) > 0 {
		invResp, err := nicoClient.GetComponentInventory(ctx, &corev1.GetComponentInventoryRequest{
			Target: &corev1.GetComponentInventoryRequest_PowerShelfIds{
				PowerShelfIds: &corev1.PowerShelfIdList{Ids: shelfIDs},
			},
		})
		if err != nil {
			log.Error().Msgf("Unable to retrieve powershelf inventory from NICo: %v", err)
		} else {
			applyInventoryToComponents(ctx, pool, invResp, componentsByShelfID)
		}
	}

	syncPowershelfStatuses(ctx, pool, nicoClient, componentsByShelfID)

	log.Info().Msgf("Powershelf NICo sync: %d drift(s) out of %d expected", len(drifts), len(expectedPowershelves))
	return received, drifts, true
}

// syncPowershelfStatuses is the power-shelf equivalent of syncSwitchStatuses.
func syncPowershelfStatuses(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
	componentsByShelfID map[string]*model.Component,
) {
	ids := mapKeys(componentsByShelfID)
	if len(ids) == 0 {
		return
	}
	statesByID, err := nicoClient.FindPowerShelfControllerStates(ctx, ids)
	if err != nil {
		log.Error().Msgf("Unable to retrieve power-shelf controller_states from NICo: %v", err)
		return
	}
	persistComponentOperationStatuses(ctx, pool, types.ComponentTypePowerShelf, statesByID, componentsByShelfID)
}
