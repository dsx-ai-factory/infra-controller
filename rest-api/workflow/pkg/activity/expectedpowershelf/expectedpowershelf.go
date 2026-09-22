// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedpowershelf

import (
	"context"
	"errors"
	"reflect"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageExpectedPowerShelf is an activity wrapper for managing ExpectedPowerShelf lifecycle that allows
// injecting DB access
type ManageExpectedPowerShelf struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateExpectedPowerShelvesInDB is a Temporal activity that takes a collection of ExpectedPowerShelf data pushed by Site Agent and updates the DB
// Expected Power Shelf records have two unique values (MAC and UUID). We ignore the MAC value and only rely on the UUID for uniqueness.
// NICo is the source of truth: out of the race-condition window we make the DB match NICo exactly.
// The reconciliation logic is as follows:
// - UUID existing in NICo but not in DB: create record in DB
// - UUID existing in both NICo and DB with differences: update record in DB
// - UUID existing in DB but not in NICo: delete record in DB
func (mei ManageExpectedPowerShelf) UpdateExpectedPowerShelvesInDB(ctx context.Context, siteID uuid.UUID, expectedPowerShelfInventory *corev1.ExpectedPowerShelfInventory) error {
	logger := log.With().Str("Activity", "UpdateExpectedPowerShelvesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if expectedPowerShelfInventory == nil {
		logger.Error().Msg("UpdateExpectedPowerShelvesInDB called with nil inventory")
		return errors.New("UpdateExpectedPowerShelvesInDB called with nil inventory")
	}

	if expectedPowerShelfInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
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

	// Initialize ExpectedPowerShelf DAO (used for reconciliation of expected instances)
	epsDAO := cdbm.NewExpectedPowerShelfDAO(mei.dbSession)

	// Fetch ALL existing expected power shelves for site
	filterInput := cdbm.ExpectedPowerShelfFilterInput{SiteIDs: []uuid.UUID{siteID}}
	existingExpectedPowerShelves, _, err := epsDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get ExpectedPowerShelves for Site from DB")
		return err
	}

	// Build a map of all existing Expected Power Shelves by UUID (unique identifier)
	existingByID := map[uuid.UUID]*cdbm.ExpectedPowerShelf{}
	for _, eps := range existingExpectedPowerShelves {
		existingByID[eps.ID] = &eps
	}

	// Track all UUIDs reported by this inventory payload (either from full list in pagination or iteration on current load)
	reportedIDs := map[uuid.UUID]bool{}

	// Track all UUIDs reported by the inventory page (if present) for use in deletion logic
	if expectedPowerShelfInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Expected Power Shelf inventory page: %d of %d, page size: %d, total count: %d",
			expectedPowerShelfInventory.InventoryPage.CurrentPage, expectedPowerShelfInventory.InventoryPage.TotalPages,
			expectedPowerShelfInventory.InventoryPage.PageSize, expectedPowerShelfInventory.InventoryPage.TotalItems)

		for _, strId := range expectedPowerShelfInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse Expected Power Shelf ID from inventory page")
				continue
			}
			reportedIDs[id] = true
		}
	}

	// iterate over current page or all (single load) if paging disabled
	for _, reps := range expectedPowerShelfInventory.GetExpectedPowerShelves() {
		if reps == nil {
			logger.Error().Msg("received nil Expected Power Shelf entry, skipping processing")
			continue
		} else if reps.ExpectedPowerShelfId == nil {
			mac := "unknown"
			if reps.BmcMacAddress != "" {
				mac = reps.BmcMacAddress
			}
			logger.Error().Str("MAC", mac).Msg("received Expected Power Shelf entry from Site without UUID set, skipping processing")
			continue
		}
		epsID, perr := uuid.Parse(reps.ExpectedPowerShelfId.Value)
		if perr != nil || epsID == uuid.Nil {
			logger.Error().Str("ID", reps.ExpectedPowerShelfId.Value).Msg("received Expected Power Shelf entry from Site with invalid UUID, skipping processing")
			continue
		}
		reportedIDs[epsID] = true

		reported := &cdbm.ExpectedPowerShelf{}
		reported.FromProto(reps)

		// Create a new Expected Power Shelf if it doesn't already exist in DB
		cur, found := existingByID[epsID]
		if !found {
			_, cerr := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
				ExpectedPowerShelfID: epsID,
				SiteID:               siteID,
				BmcMacAddress:        reported.BmcMacAddress,
				ShelfSerialNumber:    reported.ShelfSerialNumber,
				BmcIpAddress:         reported.BmcIpAddress,
				Labels:               reported.Labels,
				CreatedBy:            siteID, /* This would normally be a user ID, but that isn't something NICo provides */
			})
			if cerr != nil {
				logger.Error().Err(cerr).Str("ID", epsID.String()).Msg("failed to create ExpectedPowerShelf in DB")
			}
			continue
		}

		// A row written since the Site collected this inventory holds changes the snapshot
		// cannot know about, including any made through the API, so writing the reported values
		// over them would lose those edits.
		if site.IsTimeWithinStaleInventoryThreshold(cur.Updated) {
			logger.Info().Str("ExpectedPowerShelfID", cur.ID.String()).Msg("not updating ExpectedPowerShelf yet because it changed more recently than the inventory interval")

			continue
		}

		// update if any field differs
		if cur.BmcMacAddress != reported.BmcMacAddress ||
			cur.ShelfSerialNumber != reported.ShelfSerialNumber ||
			!util.PtrsEqual(cur.BmcIpAddress, reported.BmcIpAddress) ||
			!reflect.DeepEqual(cur.Labels, reported.Labels) {
			// nil labels in nico can mean we need to clear out existing labels in DB
			// but a nil value will not trigger an update in the DAO layer. We could use `Clear` but an empty map
			// will save a call to the DB.
			labels := reported.Labels
			if cur.Labels != nil && labels == nil {
				labels = map[string]string{}
			}
			_, uerr := epsDAO.Update(ctx, nil, cdbm.ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: cur.ID,
				BmcMacAddress:        &reported.BmcMacAddress,
				ShelfSerialNumber:    &reported.ShelfSerialNumber,
				BmcIpAddress:         reported.BmcIpAddress,
				Labels:               labels,
			})
			if uerr != nil {
				logger.Error().Err(uerr).Str("ExpectedPowerShelfID", cur.ID.String()).Msg("failed to update ExpectedPowerShelf in DB")
			}
		}
	}

	// Delete any Expected Power Shelf present in DB not present in NICo.
	// We only act if this is the last page (or paging disabled) and outside race window.
	// The source of truth for NICo is reportedIDs.
	if expectedPowerShelfInventory.InventoryPage == nil || expectedPowerShelfInventory.InventoryPage.TotalPages == 0 || (expectedPowerShelfInventory.InventoryPage.CurrentPage == expectedPowerShelfInventory.InventoryPage.TotalPages) {
		for _, eps := range existingExpectedPowerShelves {
			if _, keep := reportedIDs[eps.ID]; keep {
				continue
			}
			// Avoid destructive actions inside race-condition window
			if site.IsTimeWithinStaleInventoryThreshold(eps.Updated) {
				continue
			}
			logger.Info().Str("ExpectedPowerShelfID", eps.ID.String()).Msg("deleting ExpectedPowerShelf from DB since it was no longer reported in inventory from Site")
			if derr := epsDAO.Delete(ctx, nil, eps.ID); derr != nil {
				logger.Error().Err(derr).Str("ExpectedPowerShelfID", eps.ID.String()).Msg("failed to delete ExpectedPowerShelf from DB")
			}

		}
	}

	logger.Info().Msg("completed activity")
	return nil
}

// NewManageExpectedPowerShelf returns a new ManageExpectedPowerShelf activity
func NewManageExpectedPowerShelf(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageExpectedPowerShelf {
	return ManageExpectedPowerShelf{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
