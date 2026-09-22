// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedswitch

import (
	"context"
	"errors"
	"reflect"
	"slices"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageExpectedSwitch is an activity wrapper for managing ExpectedSwitch lifecycle that allows
// injecting DB access
type ManageExpectedSwitch struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateExpectedSwitchesInDB is a Temporal activity that takes a collection of ExpectedSwitch data pushed by Site Agent and updates the DB
// Expected Switch records have two unique values (MAC and UUID). We ignore the MAC value and only rely on the UUID for uniqueness.
// NICo is the source of truth: out of the race-condition window we make the DB match NICo exactly.
// The reconciliation logic is as follows:
// - UUID existing in NICo but not in DB: create record in DB
// - UUID existing in both NICo and DB with differences: update record in DB
// - UUID existing in DB but not in NICo: delete record in DB
func (mei ManageExpectedSwitch) UpdateExpectedSwitchesInDB(ctx context.Context, siteID uuid.UUID, expectedSwitchInventory *corev1.ExpectedSwitchInventory) error {
	logger := log.With().Str("Activity", "UpdateExpectedSwitchesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if expectedSwitchInventory == nil {
		logger.Error().Msg("UpdateExpectedSwitchesInDB called with nil inventory")
		return errors.New("UpdateExpectedSwitchesInDB called with nil inventory")
	}

	if expectedSwitchInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	// Ensure Site exists
	stDAO := cdbm.NewSiteDAO(mei.dbSession)
	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("received inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	// Initialize ExpectedSwitch DAO (used for reconciliation of expected instances)
	esDAO := cdbm.NewExpectedSwitchDAO(mei.dbSession)

	// Fetch ALL existing expected switches for site
	filterInput := cdbm.ExpectedSwitchFilterInput{SiteIDs: []uuid.UUID{siteID}}
	existingExpectedSwitches, _, err := esDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get ExpectedSwitches for Site from DB")
		return err
	}

	// Build a map of all existing Expected Switches by UUID (unique identifier)
	existingByID := map[uuid.UUID]*cdbm.ExpectedSwitch{}
	for _, es := range existingExpectedSwitches {
		existingByID[es.ID] = &es
	}

	// Track all UUIDs reported by this inventory payload (either from full list in pagination or iteration on current load)
	reportedIDs := map[uuid.UUID]bool{}

	// Track all UUIDs reported by the inventory page (if present) for use in deletion logic
	if expectedSwitchInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Expected Switch inventory page: %d of %d, page size: %d, total count: %d",
			expectedSwitchInventory.InventoryPage.CurrentPage, expectedSwitchInventory.InventoryPage.TotalPages,
			expectedSwitchInventory.InventoryPage.PageSize, expectedSwitchInventory.InventoryPage.TotalItems)

		for _, strId := range expectedSwitchInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse Expected Switch ID from inventory page")
				continue
			}
			reportedIDs[id] = true
		}
	}

	// iterate over current page or all (single load) if paging disabled
	for _, res := range expectedSwitchInventory.GetExpectedSwitches() {
		if res == nil {
			logger.Error().Msg("received nil Expected Switch entry, skipping processing")
			continue
		} else if res.ExpectedSwitchId == nil {
			mac := "unknown"
			if res.BmcMacAddress != "" {
				mac = res.BmcMacAddress
			}
			logger.Error().Str("MAC", mac).Msg("received Expected Switch entry from Site without UUID set, skipping processing")
			continue
		}
		esID, perr := uuid.Parse(res.ExpectedSwitchId.Value)
		if perr != nil || esID == uuid.Nil {
			logger.Error().Str("ID", res.ExpectedSwitchId.Value).Msg("received Expected Switch entry from Site with invalid UUID, skipping processing")
			continue
		}
		reportedIDs[esID] = true

		reported := &cdbm.ExpectedSwitch{}
		reported.FromProto(res)

		// Create a new Expected Switch if it doesn't already exist in DB
		cur, found := existingByID[esID]
		if !found {
			_, cerr := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
				ExpectedSwitchID:   esID,
				SiteID:             siteID,
				BmcMacAddress:      reported.BmcMacAddress,
				SwitchSerialNumber: reported.SwitchSerialNumber,
				NvosMacAddresses:   reported.NvosMacAddresses,
				Labels:             reported.Labels,
				CreatedBy:          siteID, /* This would normally be a user ID, but that isn't something NICo provides */
			})
			if cerr != nil {
				logger.Error().Err(cerr).Str("ID", esID.String()).Msg("failed to create ExpectedSwitch in DB")
			}
			continue
		}

		// A row written since the Site collected this inventory holds changes the snapshot
		// cannot know about, including any made through the API, so writing the reported values
		// over them would lose those edits.
		if site.IsTimeWithinStaleInventoryThreshold(cur.Updated) {
			logger.Info().Str("ExpectedSwitchID", cur.ID.String()).Msg("not updating ExpectedSwitch yet because it changed more recently than the inventory interval")

			continue
		}

		// update if any field differs
		if cur.BmcMacAddress != reported.BmcMacAddress ||
			cur.SwitchSerialNumber != reported.SwitchSerialNumber ||
			!slices.Equal(cur.NvosMacAddresses, reported.NvosMacAddresses) ||
			!reflect.DeepEqual(cur.Labels, reported.Labels) {
			// nil labels in nico can mean we need to clear out existing labels in DB
			// but a nil value will not trigger an update in the DAO layer. We could use `Clear` but an empty map
			// will save a call to the DB.
			labels := reported.Labels
			if cur.Labels != nil && labels == nil {
				labels = map[string]string{}
			}
			// nil NVOS MACs from nico follow the same rule as labels: swap in an
			// empty slice so the DAO clears a previously-set list.
			nvosMacAddresses := reported.NvosMacAddresses
			if cur.NvosMacAddresses != nil && nvosMacAddresses == nil {
				nvosMacAddresses = []string{}
			}
			_, uerr := esDAO.Update(ctx, nil, cdbm.ExpectedSwitchUpdateInput{
				ExpectedSwitchID:   cur.ID,
				BmcMacAddress:      &reported.BmcMacAddress,
				SwitchSerialNumber: &reported.SwitchSerialNumber,
				NvosMacAddresses:   nvosMacAddresses,
				Labels:             labels,
			})
			if uerr != nil {
				logger.Error().Err(uerr).Str("ExpectedSwitchID", cur.ID.String()).Msg("failed to update ExpectedSwitch in DB")
			}
		}
	}

	// Delete any Expected Switch present in DB not present in NICo.
	// We only act if this is the last page (or paging disabled) and outside race window.
	// The source of truth for NICo is reportedIDs.
	if expectedSwitchInventory.InventoryPage == nil || expectedSwitchInventory.InventoryPage.TotalPages == 0 || (expectedSwitchInventory.InventoryPage.CurrentPage == expectedSwitchInventory.InventoryPage.TotalPages) {
		for _, es := range existingExpectedSwitches {
			if _, keep := reportedIDs[es.ID]; keep {
				continue
			}
			// Avoid destructive actions inside race-condition window
			if site.IsTimeWithinStaleInventoryThreshold(es.Updated) {
				continue
			}
			logger.Info().Str("ExpectedSwitchID", es.ID.String()).Msg("deleting ExpectedSwitch from DB since it was no longer reported in inventory from Site")
			if derr := esDAO.Delete(ctx, nil, es.ID); derr != nil {
				logger.Error().Err(derr).Str("ExpectedSwitchID", es.ID.String()).Msg("failed to delete ExpectedSwitch from DB")
			}

		}
	}

	logger.Info().Msg("completed activity")
	return nil
}

// NewManageExpectedSwitch returns a new ManageExpectedSwitch activity
func NewManageExpectedSwitch(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageExpectedSwitch {
	return ManageExpectedSwitch{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
