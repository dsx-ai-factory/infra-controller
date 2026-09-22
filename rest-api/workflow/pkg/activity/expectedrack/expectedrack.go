// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

import (
	"context"
	"errors"
	"reflect"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageExpectedRack is an activity wrapper for managing ExpectedRack lifecycle that allows
// injecting DB access
type ManageExpectedRack struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateExpectedRacksInDB is a Temporal activity that takes a collection of ExpectedRack data pushed by Site Agent and updates the DB
// ExpectedRacks are uniquely identified per Site by rack_id (operator-supplied string).
// NICo is the source of truth: out of the race-condition window we make the DB match NICo exactly.
// The reconciliation logic is as follows:
// - rack_id existing in NICo but not in DB: create record in DB
// - rack_id existing in both NICo and DB with differences: update record in DB
// - rack_id existing in DB but not in NICo: delete record in DB
func (mer ManageExpectedRack) UpdateExpectedRacksInDB(ctx context.Context, siteID uuid.UUID, expectedRackInventory *corev1.ExpectedRackInventory) error {
	logger := log.With().Str("Activity", "UpdateExpectedRacksInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if expectedRackInventory == nil {
		logger.Error().Msg("UpdateExpectedRacksInDB called with nil inventory")
		return errors.New("UpdateExpectedRacksInDB called with nil inventory")
	}

	if expectedRackInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	// Ensure Site exists
	stDAO := cdbm.NewSiteDAO(mer.dbSession)
	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("received inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	// Initialize ExpectedRack DAO
	erDAO := cdbm.NewExpectedRackDAO(mer.dbSession)

	// Fetch ALL existing expected racks for site
	filterInput := cdbm.ExpectedRackFilterInput{SiteIDs: []uuid.UUID{siteID}}
	existingExpectedRacks, _, err := erDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get ExpectedRacks for Site from DB")
		return err
	}

	// Build a map of all existing Expected Racks by RackID (operator-supplied identifier, unique per site)
	existingByRackID := map[string]*cdbm.ExpectedRack{}
	for i := range existingExpectedRacks {
		er := &existingExpectedRacks[i]
		existingByRackID[er.RackID] = er
	}

	// Track all RackIDs reported by this inventory payload
	reportedRackIDs := map[string]bool{}

	// Track all RackIDs reported by the inventory page (if present) for use in deletion logic
	if expectedRackInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Expected Rack inventory page: %d of %d, page size: %d, total count: %d",
			expectedRackInventory.InventoryPage.CurrentPage, expectedRackInventory.InventoryPage.TotalPages,
			expectedRackInventory.InventoryPage.PageSize, expectedRackInventory.InventoryPage.TotalItems)

		for _, rackID := range expectedRackInventory.InventoryPage.ItemIds {
			if rackID == "" {
				continue
			}
			reportedRackIDs[rackID] = true
		}
	}

	// iterate over current page or all (single load) if paging disabled
	for _, rer := range expectedRackInventory.GetExpectedRacks() {
		if rer == nil {
			logger.Error().Msg("received nil Expected Rack entry, skipping processing")
			continue
		}
		if rer.RackId == nil || rer.RackId.Id == "" {
			logger.Error().Msg("received Expected Rack entry from Site without rack_id set, skipping processing")
			continue
		}
		rackID := rer.RackId.Id
		reportedRackIDs[rackID] = true

		reported := &cdbm.ExpectedRack{}
		reported.FromProto(rer)

		// Create a new Expected Rack if it doesn't already exist in DB
		cur, found := existingByRackID[rackID]
		if !found {
			_, cerr := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
				ExpectedRackID: uuid.New(),
				SiteID:         siteID,
				RackID:         reported.RackID,
				RackProfileID:  reported.RackProfileID,
				Name:           reported.Name,
				Description:    reported.Description,
				Labels:         reported.Labels,
				CreatedBy:      siteID, /* This would normally be a user ID, but that isn't something NICo provides */
			})
			if cerr != nil {
				logger.Error().Err(cerr).Str("RackID", rackID).Msg("failed to create ExpectedRack in DB")
			}
			continue
		}

		// A row written since the Site collected this inventory holds changes the snapshot
		// cannot know about, including any made through the API, so writing the reported values
		// over them would lose those edits.
		if site.IsTimeWithinStaleInventoryThreshold(cur.Updated) {
			logger.Info().Str("ExpectedRackID", cur.ID.String()).Msg("not updating ExpectedRack yet because it changed more recently than the inventory interval")

			continue
		}

		// update if any field differs
		if cur.RackProfileID != reported.RackProfileID ||
			cur.Name != reported.Name ||
			cur.Description != reported.Description ||
			!reflect.DeepEqual(cur.Labels, reported.Labels) {
			// nil labels in nico can mean we need to clear out existing labels in DB.
			// A nil value will not trigger an update in the DAO layer, so use an empty map.
			labels := reported.Labels
			if cur.Labels != nil && labels == nil {
				labels = map[string]string{}
			}
			_, uerr := erDAO.Update(ctx, nil, cdbm.ExpectedRackUpdateInput{
				ExpectedRackID: cur.ID,
				RackProfileID:  &reported.RackProfileID,
				Name:           &reported.Name,
				Description:    &reported.Description,
				Labels:         labels,
			})
			if uerr != nil {
				logger.Error().Err(uerr).Str("ExpectedRackID", cur.ID.String()).Str("RackID", rackID).Msg("failed to update ExpectedRack in DB")
			}
		}
	}

	// Delete any Expected Rack present in DB not present in NICo.
	// We only act if this is the last page (or paging disabled) and outside race window.
	// The source of truth for NICo is reportedRackIDs.
	if expectedRackInventory.InventoryPage == nil || expectedRackInventory.InventoryPage.TotalPages == 0 || (expectedRackInventory.InventoryPage.CurrentPage == expectedRackInventory.InventoryPage.TotalPages) {
		for _, er := range existingExpectedRacks {
			if _, keep := reportedRackIDs[er.RackID]; keep {
				continue
			}
			// Avoid destructive actions inside race-condition window
			if site.IsTimeWithinStaleInventoryThreshold(er.Updated) {
				continue
			}
			logger.Info().Str("ExpectedRackID", er.ID.String()).Str("RackID", er.RackID).Msg("deleting ExpectedRack from DB since it was no longer reported in inventory from Site")
			if derr := erDAO.Delete(ctx, nil, er.ID); derr != nil {
				logger.Error().Err(derr).Str("ExpectedRackID", er.ID.String()).Msg("failed to delete ExpectedRack from DB")
			}
		}
	}

	logger.Info().Msg("completed activity")
	return nil
}

// NewManageExpectedRack returns a new ManageExpectedRack activity
func NewManageExpectedRack(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageExpectedRack {
	return ManageExpectedRack{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
