// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrackgroup

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

// ManageExpectedRackGroup is an activity wrapper for managing ExpectedRackGroup lifecycle that allows
// injecting DB access
type ManageExpectedRackGroup struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateExpectedRackGroupsInDB is a Temporal activity that takes a collection of ExpectedRackGroup data pushed by Site Agent and updates the DB
// ExpectedRackGroups are uniquely identified per Site by rack_group_id (operator-supplied string).
// NICo is the source of truth: out of the race-condition window we make the DB match NICo exactly.
// The reconciliation logic is as follows:
// - rack_group_id existing in NICo but not in DB: create record in DB
// - rack_group_id existing in both NICo and DB with differences: update record in DB
// - rack_group_id existing in DB but not in NICo: delete record in DB
func (mer ManageExpectedRackGroup) UpdateExpectedRackGroupsInDB(ctx context.Context, siteID uuid.UUID, expectedRackGroupInventory *corev1.ExpectedRackGroupInventory) error {
	logger := log.With().Str("Activity", "UpdateExpectedRackGroupsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if expectedRackGroupInventory == nil {
		logger.Error().Msg("UpdateExpectedRackGroupsInDB called with nil inventory")
		return errors.New("UpdateExpectedRackGroupsInDB called with nil inventory")
	}

	if expectedRackGroupInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}
	if expectedRackGroupInventory.InventoryStatus != corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS {
		return errors.New("expected rack group inventory must have a successful status before reconciliation")
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

	// Initialize ExpectedRackGroup DAO
	erDAO := cdbm.NewExpectedRackGroupDAO(mer.dbSession)

	// Fetch all existing expected rack groups for the Site.
	filterInput := cdbm.ExpectedRackGroupFilterInput{SiteIDs: []uuid.UUID{siteID}}
	existingExpectedRackGroups, _, err := erDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get ExpectedRackGroups for Site from DB")
		return err
	}

	// Build a map of all existing Expected Rack Groups by RackGroupID (operator-supplied identifier, unique per site)
	existingByRackGroupID := map[string]*cdbm.ExpectedRackGroup{}
	for i := range existingExpectedRackGroups {
		er := &existingExpectedRackGroups[i]
		existingByRackGroupID[er.RackGroupID] = er
	}

	// Track all RackGroupIDs reported by this inventory payload
	reportedRackGroupIDs := map[string]bool{}

	// Track all RackGroupIDs reported by the inventory page (if present) for use in deletion logic
	if expectedRackGroupInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Expected Rack Group inventory page: %d of %d, page size: %d, total count: %d",
			expectedRackGroupInventory.InventoryPage.CurrentPage, expectedRackGroupInventory.InventoryPage.TotalPages,
			expectedRackGroupInventory.InventoryPage.PageSize, expectedRackGroupInventory.InventoryPage.TotalItems)

		for _, rackGroupID := range expectedRackGroupInventory.InventoryPage.ItemIds {
			if rackGroupID == "" {
				continue
			}
			reportedRackGroupIDs[rackGroupID] = true
		}
	}

	// iterate over current page or all (single load) if paging disabled
	for _, rer := range expectedRackGroupInventory.GetExpectedRackGroups() {
		if rer == nil {
			logger.Error().Msg("received nil Expected Rack Group entry, skipping processing")
			continue
		}
		if rer.RackGroupId == nil || rer.RackGroupId.Id == "" {
			logger.Error().Msg("received Expected Rack Group entry from Site without rack_group_id set, skipping processing")
			continue
		}
		rackGroupID := rer.RackGroupId.Id
		reportedRackGroupIDs[rackGroupID] = true

		reported := &cdbm.ExpectedRackGroup{}
		if err := reported.FromProto(rer); err != nil {
			return err
		}

		// Create a new Expected Rack Group if it doesn't already exist in DB
		cur, found := existingByRackGroupID[rackGroupID]
		if !found {
			_, cerr := erDAO.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{
				ExpectedRackGroupID: uuid.New(),
				SiteID:              siteID,
				RackGroupID:         reported.RackGroupID,
				Topology:            reported.Topology,
				Racks:               reported.Racks,
				Name:                reported.Name,
				Description:         reported.Description,
				Labels:              reported.Labels,
				CreatedBy:           siteID, /* This would normally be a user ID, but that isn't something NICo provides */
			})
			if cerr != nil {
				logger.Error().Err(cerr).Str("RackGroupID", rackGroupID).Msg("failed to create ExpectedRackGroup in DB")
				return cerr
			}
			continue
		}

		// A row written since the Site collected this inventory holds changes the snapshot
		// cannot know about, including any made through the API, so writing the reported values
		// over them would lose those edits.
		if site.IsTimeWithinStaleInventoryThreshold(cur.Updated) {
			logger.Info().Str("ExpectedRackGroupID", cur.ID.String()).Msg("not updating ExpectedRackGroup yet because it changed more recently than the inventory interval")

			continue
		}

		// update if any field differs
		if cur.Topology != reported.Topology ||
			!reflect.DeepEqual(cur.Racks, reported.Racks) ||
			cur.Name != reported.Name ||
			cur.Description != reported.Description ||
			!reflect.DeepEqual(cur.Labels, reported.Labels) {
			// nil labels in nico can mean we need to clear out existing labels in DB.
			// A nil value will not trigger an update in the DAO layer, so use an empty map.
			labels := reported.Labels
			if cur.Labels != nil && labels == nil {
				labels = map[string]string{}
			}
			_, uerr := erDAO.Update(ctx, nil, cdbm.ExpectedRackGroupUpdateInput{
				ExpectedRackGroupID: cur.ID,
				ExpectedUpdated:     &cur.Updated,
				Topology:            &reported.Topology,
				Racks:               reported.Racks,
				Name:                &reported.Name,
				Description:         &reported.Description,
				Labels:              labels,
			})
			if uerr != nil {
				if errors.Is(uerr, cdb.ErrDoesNotExist) {
					// A concurrent write or deletion superseded the version we read.
					continue
				}
				logger.Error().Err(uerr).Str("ExpectedRackGroupID", cur.ID.String()).Str("RackGroupID", rackGroupID).Msg("failed to update ExpectedRackGroup in DB")
				return uerr
			}
		}
	}

	// Delete any Expected Rack Group present in DB not present in NICo.
	// We only act if this is the last page (or paging disabled) and outside race window.
	// The source of truth for NICo is reportedRackGroupIDs.
	if expectedRackGroupInventory.InventoryPage == nil || expectedRackGroupInventory.InventoryPage.TotalPages == 0 || (expectedRackGroupInventory.InventoryPage.CurrentPage == expectedRackGroupInventory.InventoryPage.TotalPages) {
		for _, er := range existingExpectedRackGroups {
			if _, keep := reportedRackGroupIDs[er.RackGroupID]; keep {
				continue
			}
			// Avoid destructive actions inside race-condition window
			if site.IsTimeWithinStaleInventoryThreshold(er.Updated) {
				continue
			}
			deleted, derr := erDAO.DeleteIfUnchanged(ctx, nil, er.ID, er.Updated)
			if derr != nil {
				logger.Error().Err(derr).Str("ExpectedRackGroupID", er.ID.String()).Msg("failed to delete ExpectedRackGroup from DB")
				return derr
			}
			if deleted {
				logger.Info().Str("ExpectedRackGroupID", er.ID.String()).Str("RackGroupID", er.RackGroupID).Msg("deleted ExpectedRackGroup no longer reported in inventory from Site")
			}
		}
	}

	logger.Info().Msg("completed activity")
	return nil
}

// NewManageExpectedRackGroup returns a new ManageExpectedRackGroup activity
func NewManageExpectedRackGroup(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageExpectedRackGroup {
	return ManageExpectedRackGroup{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
