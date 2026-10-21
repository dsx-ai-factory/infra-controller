// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sku

import (
	"context"
	"errors"
	"reflect"
	"slices"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageSku is an activity wrapper for managing SKU inventory that allows injecting DB access
type ManageSku struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// UpdateSkusInDB is a Temporal activity that reconciles SKU inventory pushed by
// the Site Agent into the REST database.
func (ms ManageSku) UpdateSkusInDB(ctx context.Context, siteID uuid.UUID, skuInventory *corev1.SkuInventory) error {
	logger := log.With().Str("Activity", "UpdateSkusInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if skuInventory == nil {
		logger.Error().Msg("UpdateSkusInDB called with nil inventory")
		return errors.New("UpdateSkusInDB called with nil inventory")
	}

	if skuInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	// Ensure Site exists
	stDAO := cdbm.NewSiteDAO(ms.dbSession)
	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("received inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	// Initialize DAO
	skuDAO := cdbm.NewSkuDAO(ms.dbSession)

	// Fetch ALL existing SKUs for site
	filterInput := cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{siteID}}
	existingSkus, _, err := skuDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)})
	if err != nil {
		logger.Error().Err(err).Msg("failed to get SKUs for Site from DB")
		return err
	}

	// Build a map of all existing SKUs by ID (unique identifier)
	existingByID := map[string]*cdbm.SKU{}
	for _, sku := range existingSkus {
		existingByID[sku.ID] = &sku
	}

	// Track all IDs reported by this inventory payload (either from full list in pagination or iteration on current load)
	reportedIDs := map[string]bool{}

	// Track all IDs reported by the inventory page (if present) for use in deletion logic
	if skuInventory.InventoryPage != nil {
		logger.Info().Msgf("Received SKU inventory page: %d of %d, page size: %d, total count: %d",
			skuInventory.InventoryPage.CurrentPage, skuInventory.InventoryPage.TotalPages,
			skuInventory.InventoryPage.PageSize, skuInventory.InventoryPage.TotalItems)

		for _, strId := range skuInventory.InventoryPage.ItemIds {
			reportedIDs[strId] = true
		}
	}

	// iterate over current page or all (single load) if paging disabled
	for _, reportedSku := range skuInventory.GetSkus() {
		if reportedSku == nil {
			logger.Error().Msg("received nil SKU entry, skipping processing")
			continue
		} else if reportedSku.Id == "" {
			logger.Error().Msg("received SKU entry from Site with empty ID, skipping processing")
			continue
		}
		reportedIDs[reportedSku.Id] = true

		reported := &cdbm.SKU{}
		reported.FromProto(reportedSku, siteID)

		// Create a new SKU if it doesn't already exist in DB
		cur, found := existingByID[reportedSku.Id]
		if !found {
			var created *time.Time
			if !reported.Created.IsZero() {
				created = &reported.Created
			}
			// Create new SKU with SiteID
			sku := cdbm.SkuCreateInput{
				SkuID:                reported.ID,
				SiteID:               reported.SiteID,
				Created:              created,
				Description:          reported.Description,
				SchemaVersion:        reported.SchemaVersion,
				DeviceType:           reported.DeviceType,
				Components:           reported.Components,
				AssociatedMachineIds: reported.AssociatedMachineIds,
			}
			_, cerr := skuDAO.Create(ctx, nil, sku)
			if cerr != nil {
				logger.Error().Err(cerr).Str("SkuID", reportedSku.Id).Msg("failed to create SKU in DB")
			}
			continue
		}

		// Updated is the row's own write time, so a SKU written since the Site collected this
		// inventory carries changes the snapshot cannot know about, including any made through
		// the API. Writing the reported values over them would lose those edits.
		if site.IsTimeWithinStaleInventoryThreshold(cur.Updated) {
			logger.Info().Str("SkuId", cur.ID).Msg("not updating SKU yet because it changed more recently than the inventory interval")

			continue
		}

		// A missing `Components` value clears existing components. Compare the
		// same empty wrapper the DAO writes so an already empty row stays unchanged.
		components := reported.Components
		if cur.Components != nil && components == nil {
			components = &cdbm.SkuComponents{SkuComponents: &corev1.SkuComponents{}}
		}
		// The DAO treats nil `DeviceType` as omitted, so it preserves the stored type.
		deviceTypeChanged := reported.DeviceType != nil && !reflect.DeepEqual(cur.DeviceType, reported.DeviceType)
		createdChanged := !reported.Created.IsZero() && !cur.Created.Equal(reported.Created)
		if createdChanged || cur.Description != reported.Description || cur.SchemaVersion != reported.SchemaVersion ||
			!cur.Components.Equal(components) || deviceTypeChanged ||
			!slices.Equal(cur.AssociatedMachineIds, reported.AssociatedMachineIds) {
			// nil AssociatedMachineIds in nico can mean we need to clear out existing AssociatedMachineIds in DB
			// but a nil value will not trigger an update in the DAO layer. We could use `Clear` but an empty map
			// will save a call to the DB.
			associated := reported.AssociatedMachineIds
			if cur.AssociatedMachineIds != nil && associated == nil {
				associated = []string{}
			}
			var created *time.Time
			if createdChanged {
				created = &reported.Created
			}
			sku := cdbm.SkuUpdateInput{
				SkuID:                reported.ID,
				ExpectedUpdated:      &cur.Updated,
				Created:              created,
				Description:          &reported.Description,
				SchemaVersion:        &reported.SchemaVersion,
				Components:           components,
				DeviceType:           reported.DeviceType,
				AssociatedMachineIds: associated,
			}
			_, uerr := skuDAO.Update(ctx, nil, sku)
			if uerr != nil {
				if errors.Is(uerr, cdb.ErrDoesNotExist) {
					// A concurrent write or deletion superseded the version we read.
					continue
				}
				logger.Error().Err(uerr).Str("SkuID", reportedSku.Id).Msg("failed to update SKU in DB")
			}
		}
	}

	// Delete stale records missing from `reportedIDs` only after the final page
	// (or when paging is disabled). Matching the saved `Updated` timestamp in
	// `DeleteIfUnchanged` preserves API edits committed after our initial read.
	if util.ShouldReconcileDeletions(skuInventory.GetInventoryPage()) {
		for _, sk := range existingSkus {
			if _, keep := reportedIDs[sk.ID]; keep {
				continue
			}
			// Updated tracks when this row was last written, unlike Created which carries Core's
			// timestamp and stays old for a SKU that Cloud only just learned about. A SKU
			// written since the Site collected this inventory may be absent from it only because
			// the snapshot predates the row, so deleting would drop a SKU that just arrived.
			if site.IsTimeWithinStaleInventoryThreshold(sk.Updated) {
				logger.Info().Str("SkuId", sk.ID).Msg("not deleting SKU yet because it is newer than the inventory interval")

				continue
			}
			deleted, derr := skuDAO.DeleteIfUnchanged(ctx, nil, sk.ID, sk.Updated)
			if derr != nil {
				logger.Error().Err(derr).Str("SkuID", sk.ID).Msg("failed to delete SKU from DB")
				continue
			}
			if deleted {
				logger.Info().Str("SkuId", sk.ID).Msg("deleted SKU no longer reported in inventory from Site")
			}
		}
	}

	logger.Info().Msg("completed activity")
	return nil
}

// NewManageSku returns a new ManageSku activity
func NewManageSku(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageSku {
	return ManageSku{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
