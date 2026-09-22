// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvlinklogicalpartition

import (
	"context"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageNVLinkLogicalPartition is an activity wrapper for managing NVLinkLogicalPartition lifecycle that allows
// injecting DB access
type ManageNVLinkLogicalPartition struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions
// UpdateNVLinkLogicalPartitionsInDB is a Temporal activity that takes a collection of NVLinkPartition data pushed by Site Agent and updates the DB
func (mnlp ManageNVLinkLogicalPartition) UpdateNVLinkLogicalPartitionsInDB(ctx context.Context, siteID uuid.UUID, nvlinklogicalpartitionInventory *corev1.NVLinkLogicalPartitionInventory) error {
	logger := log.With().Str("Activity", "UpdateNVLinkLogicalPartitionsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mnlp.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received NVLink Logical Partition inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if nvlinklogicalpartitionInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	nvlinklogicalpartitionDAO := cdbm.NewNVLinkLogicalPartitionDAO(mnlp.dbSession)

	existingNVLinkLogicalPartitions, _, err := nvlinklogicalpartitionDAO.GetAll(
		ctx,
		nil,
		cdbm.NVLinkLogicalPartitionFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get existing NVLink Logical Partitions for Site from DB")
		return err
	}

	// Construct a map of Controller NVLinkPartition ID to NVLinkPartition
	existingNVLinkLogicalPartitionIDMap := make(map[string]*cdbm.NVLinkLogicalPartition)

	for _, nvllp := range existingNVLinkLogicalPartitions {
		curNVLLP := nvllp
		existingNVLinkLogicalPartitionIDMap[nvllp.ID.String()] = &curNVLLP
	}

	reportedNVLinkLogicalPartitionIDMap := map[uuid.UUID]bool{}

	if nvlinklogicalpartitionInventory.InventoryPage != nil {
		logger.Info().Msgf("Received NVLink Logical Partition inventory page: %d of %d, page size: %d, total count: %d",
			nvlinklogicalpartitionInventory.InventoryPage.CurrentPage, nvlinklogicalpartitionInventory.InventoryPage.TotalPages,
			nvlinklogicalpartitionInventory.InventoryPage.PageSize, nvlinklogicalpartitionInventory.InventoryPage.TotalItems)

		for _, strId := range nvlinklogicalpartitionInventory.InventoryPage.ItemIds {
			id, err := uuid.Parse(strId)
			if err != nil {
				logger.Error().Err(err).Str("ID", strId).Msg("failed to parse NVLink Logical Partition ID from inventory page")
				continue
			}
			reportedNVLinkLogicalPartitionIDMap[id] = true
		}
	}

	statusDetailDAO := cdbm.NewStatusDetailDAO(mnlp.dbSession)
	// Iterate through NVLinkPartition Inventory and update DB
	for _, controllerNvllp := range nvlinklogicalpartitionInventory.Partitions {
		slogger := logger.With().Str("NVLink Logical Partition ID", controllerNvllp.Id.Value).Logger()

		nvllp, ok := existingNVLinkLogicalPartitionIDMap[controllerNvllp.Id.Value]
		if !ok {
			// TODO: Since Site is the source of truth, we must auto-create any Partitions that are in the Site inventory but not in the DB
			slogger.Error().Str("NVLink Logical Partition ID", controllerNvllp.Id.Value).Msg("NVLink Logical Partition does not have a record in DB, possibly created directly on Site")
			continue
		}

		reportedNVLinkLogicalPartitionIDMap[nvllp.ID] = true

		var name *string
		var description *string
		var status *cdbm.NVLinkLogicalPartitionStatus
		var statusMessage *string

		// Reset missing flag if necessary
		var isMissingOnSite *bool
		if nvllp.IsMissingOnSite {
			isMissingOnSite = cutil.GetPtr(false)
		}

		if controllerNvllp.Config != nil && controllerNvllp.Config.Metadata != nil {
			if controllerNvllp.Config.Metadata.Name != nvllp.Name {
				name = cutil.GetPtr(controllerNvllp.Config.Metadata.Name)
			}

			if nvllp.Description == nil || (nvllp.Description != nil && controllerNvllp.Config.Metadata.Description != *nvllp.Description) {
				description = cutil.GetPtr(controllerNvllp.Config.Metadata.Description)
			}
		}

		// Update status if necessary
		if controllerNvllp.Status != nil {
			if nvllp.Status == cdbm.NVLinkLogicalPartitionStatusDeleting {
				continue
			}

			var mapped cdbm.NVLinkLogicalPartitionStatus
			mapped.FromProto(controllerNvllp.Status.State)
			if mapped != "" && mapped != nvllp.Status {
				status = &mapped
				message := mapped.Message()
				statusMessage = &message
			}
		}

		needsUpdate := name != nil || description != nil || isMissingOnSite != nil || status != nil

		// A row written since the Site collected this inventory holds changes the snapshot cannot
		// know about, including any made through the API, so writing the reported values over
		// them would lose those edits.
		if needsUpdate && site.IsTimeWithinStaleInventoryThreshold(nvllp.Updated) {
			slogger.Info().Msg("not updating NVLinkLogicalPartition yet because it changed more recently than the inventory interval")

			continue
		}

		if needsUpdate {
			_, err := nvlinklogicalpartitionDAO.Update(
				ctx,
				nil,
				cdbm.NVLinkLogicalPartitionUpdateInput{
					NVLinkLogicalPartitionID: nvllp.ID,
					Name:                     name,
					Description:              description,
					IsMissingOnSite:          isMissingOnSite,
					Status:                   status,
				},
			)

			if err != nil {
				slogger.Error().Err(err).Msg("failed to update NVLink Logical Partition data in DB")
				continue
			}

		}

		if status != nil {
			_, err = statusDetailDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: nvllp.ID.String(), Status: string(*status), Message: statusMessage})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to create status detail for NVLink Logical Partition in DB")
				continue
			}
		}

	}

	// Populate list of ibps that were not found
	nvlinklogicalpartitionsToDelete := []*cdbm.NVLinkLogicalPartition{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if nvlinklogicalpartitionInventory.InventoryPage == nil || nvlinklogicalpartitionInventory.InventoryPage.TotalPages == 0 || (nvlinklogicalpartitionInventory.InventoryPage.CurrentPage == nvlinklogicalpartitionInventory.InventoryPage.TotalPages) {
		for _, nvllp := range existingNVLinkLogicalPartitionIDMap {
			found := false

			_, found = reportedNVLinkLogicalPartitionIDMap[nvllp.ID]
			if !found {
				// The NVLinkLogicalPartition was not found in the NVLinkLogicalPartition Inventory, so add it to list of NVLinkLogicalPartition to potentially delete
				nvlinklogicalpartitionsToDelete = append(nvlinklogicalpartitionsToDelete, nvllp)
			}
		}
	}

	// Loop through nvllp for deletion
	for _, nvllp := range nvlinklogicalpartitionsToDelete {
		slogger := logger.With().Str("NVLink Logical Partition ID", nvllp.ID.String()).Logger()

		if site.IsTimeWithinStaleInventoryThreshold(nvllp.Updated) {
			continue
		}

		// If the NVLinkLogicalPartition was already being deleted, we can proceed with removing it from the DB
		if nvllp.Status == cdbm.NVLinkLogicalPartitionStatusDeleting {
			err := nvlinklogicalpartitionDAO.Delete(ctx, nil, nvllp.ID)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to delete NVLink Logical Partition from DB")
			}
		} else {
			// Set isMissingOnSite flag to true and update status, user can decide on deletion
			_, err := nvlinklogicalpartitionDAO.Update(
				ctx,
				nil,
				cdbm.NVLinkLogicalPartitionUpdateInput{
					NVLinkLogicalPartitionID: nvllp.ID,
					IsMissingOnSite:          cutil.GetPtr(true),
				},
			)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to set missing on Site flag in DB for NVLink Logical Partition")
				continue
			}

			_, _, err = util.UpdateNVLinkLogicalPartitionStatusInDB(ctx, nil, mnlp.dbSession, nvllp.ID, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusError), cutil.GetPtr("NVLink Logical Partition is missing on Site"))
			if err != nil {
				slogger.Error().Err(err).Msg("failed to update NVLink Logical Partition status detail in DB")
			}
		}
	}

	return nil
}

// NewManageNVLinkLogicalPartition returns a new ManageNVLinkLogicalPartition activity
func NewManageNVLinkLogicalPartition(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageNVLinkLogicalPartition {
	return ManageNVLinkLogicalPartition{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
