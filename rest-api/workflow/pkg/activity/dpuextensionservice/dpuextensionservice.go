// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// DpuExtensionServiceTimeFormat is the time format used on Site for version info creation time
	DpuExtensionServiceTimeFormat = "2006-01-02 15:04:05.000000 UTC"
)

// ManageDpuExtensionService is an activity wrapper for managing Dpu Extension Service lifecycle that allows
// injecting DB access
type ManageDpuExtensionService struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions
// UpdateDpuExtensionServicesInDB is a Temporal activity that takes a collection of Dpu Extension Service data pushed by Site Agent and updates the DB
func (mde ManageDpuExtensionService) UpdateDpuExtensionServicesInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.DpuExtensionServiceInventory) error {
	logger := log.With().Str("Activity", "UpdateDpuExtensionServicesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("Starting activity")

	stDAO := cdbm.NewSiteDAO(mde.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(mde.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received DPU Extension Service inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")

		if inventory.StatusMsg != "" {
			err = fmt.Errorf("Site Agent inventory collection failure: %s", inventory.StatusMsg)
		} else {
			err = errors.New("Site Agent reported unknown inventory collection failure")
		}

		return temporal.NewNonRetryableApplicationError(err.Error(), util.ErrTypeSiteAgentInventoryCollectionFailure, err)
	}

	dpuExtensionServiceDAO := cdbm.NewDpuExtensionServiceDAO(mde.dbSession)
	existingDpuExtensionServices, _, err := dpuExtensionServiceDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get DPU Extension Services for Site from DB")
		return err
	}

	// Construct a map of Controller Dpu Extension Service ID to Dpu Extension Service
	existingDpuExtensionServiceIDMap := make(map[string]*cdbm.DpuExtensionService)
	for _, dpuExtensionService := range existingDpuExtensionServices {
		curDpuExtensionService := dpuExtensionService
		existingDpuExtensionServiceIDMap[dpuExtensionService.ID.String()] = &curDpuExtensionService
	}

	reportedDpuExtensionServiceIDMap := map[uuid.UUID]bool{}
	if inventory.InventoryPage != nil {
		logger.Info().Msgf("Received DPU Extension Service inventory page: %d of %d, page size: %d, total count: %d",
			inventory.InventoryPage.CurrentPage, inventory.InventoryPage.TotalPages,
			inventory.InventoryPage.PageSize, inventory.InventoryPage.TotalItems)

		for _, strId := range inventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("Controller DPU Extension Service ID", strId).Msg("failed to parse DPU Extension Service ID from inventory item IDs")
				continue
			}
			reportedDpuExtensionServiceIDMap[id] = true
		}
	}

	// Iterate through DPU Extension Service Inventory and update DB
	for _, controllerDpuExtensionService := range inventory.DpuExtensionServices {
		slogger := logger.With().Str("Controller DPU Extension Service ID", controllerDpuExtensionService.ServiceId).Logger()

		dpuExtensionService := existingDpuExtensionServiceIDMap[controllerDpuExtensionService.ServiceId]
		if dpuExtensionService == nil {
			slogger.Warn().Msg("DPU Extension Service does not have a record in DB, possibly created directly on Site")
			// TODO: Create a new DPU Extension Service record in DB
			continue
		}

		reportedDpuExtensionServiceIDMap[dpuExtensionService.ID] = true

		var status *string
		var statusMessage *string
		var isMissingOnSite *bool

		// Update DPU Extension Service status if necessary
		if dpuExtensionService.Status == cdbm.DpuExtensionServiceStatusPending {
			// If the DPU Extension Service is in Pending status, set it to Ready
			status = cutil.GetPtr(cdbm.DpuExtensionServiceStatusReady)
			statusMessage = cutil.GetPtr("DPU Extension Service is ready for deployment")
		} else if dpuExtensionService.IsMissingOnSite && dpuExtensionService.Status == cdbm.DpuExtensionServiceStatusError {
			// If the DPU Extension Service was previously missing on Site, set it back to Ready
			status = cutil.GetPtr(cdbm.DpuExtensionServiceStatusReady)
			statusMessage = cutil.GetPtr("DPU Extension Service was re-detected on Site")
			isMissingOnSite = cutil.GetPtr(false)
		}

		var version *string
		var versionInfo *cdbm.DpuExtensionServiceVersionInfo

		if controllerDpuExtensionService.LatestVersionInfo != nil {
			latestVersion := controllerDpuExtensionService.LatestVersionInfo.Version
			data := controllerDpuExtensionService.LatestVersionInfo.Data
			hasCredentials := controllerDpuExtensionService.LatestVersionInfo.HasCredential
			controllerObservability := controllerDpuExtensionService.GetLatestVersionInfo().Observability
			var dbObservability *corev1.DpuExtensionServiceObservability
			if dpuExtensionService.VersionInfo != nil && dpuExtensionService.VersionInfo.Observability != nil {
				dbObservability = dpuExtensionService.VersionInfo.Observability.DpuExtensionServiceObservability
			}

			created, err := time.Parse(DpuExtensionServiceTimeFormat, controllerDpuExtensionService.LatestVersionInfo.Created)
			if err != nil {
				if controllerDpuExtensionService.LatestVersionInfo.Created != "" {
					slogger.Error().Err(err).Str("Created", controllerDpuExtensionService.LatestVersionInfo.Created).Msg("failed to parse timestamp for version info")
				}
				created = dpuExtensionService.Updated
			}

			if dpuExtensionService.Version != nil && *dpuExtensionService.Version != latestVersion {
				version = cutil.GetPtr(latestVersion)
			}

			if dpuExtensionService.VersionInfo == nil ||
				dpuExtensionService.VersionInfo.Version != latestVersion ||
				dpuExtensionService.VersionInfo.Data != data ||
				dpuExtensionService.VersionInfo.HasCredentials != hasCredentials ||
				dpuExtensionService.VersionInfo.Created != created ||
				!proto.Equal(dbObservability, controllerObservability) {

				var observability *cdbm.DpuExtensionServiceObservability
				// If response from -core is non-nil, wrap it so we can store it.
				if controllerObservability != nil {
					observability = &cdbm.DpuExtensionServiceObservability{
						DpuExtensionServiceObservability: controllerObservability,
					}
				}

				versionInfo = &cdbm.DpuExtensionServiceVersionInfo{
					Version:        latestVersion,
					Data:           data,
					HasCredentials: hasCredentials,
					Created:        created,
					Observability:  observability,
				}
			}

		}

		var activeVersions []string
		if controllerDpuExtensionService.ActiveVersions != nil && !slices.Equal(dpuExtensionService.ActiveVersions, controllerDpuExtensionService.ActiveVersions) {
			activeVersions = controllerDpuExtensionService.ActiveVersions
		}

		needsUpdate := status != nil ||
			isMissingOnSite != nil ||
			version != nil ||
			versionInfo != nil ||
			activeVersions != nil

		// VersionInfo carries the Data, Credentials and Observability an API update sets, so a
		// row written since the Site collected this inventory holds changes the snapshot cannot
		// know about and writing the reported values over them would lose those edits.
		if needsUpdate && site.IsTimeWithinStaleInventoryThreshold(dpuExtensionService.Updated) {
			slogger.Info().Msg("not updating DpuExtensionService yet because it changed more recently than the inventory interval")

			continue
		}

		if needsUpdate {
			_, err := dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dpuExtensionService.ID,
				Version:               version,
				VersionInfo:           versionInfo,
				ActiveVersions:        activeVersions,
				Status:                status,
				IsMissingOnSite:       isMissingOnSite,
			})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to update DPU Extension Service in DB")
				continue
			}
		}

		// If status was updated, then create status detail
		if status != nil {
			_, err = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: dpuExtensionService.ID.String(), Status: *status, Message: statusMessage})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to create status detail for DPU Extension Service in DB")
				continue
			}
		}
	}

	// Populate list of DPU Extension Services that were not found
	dpuExtensionServicesToDelete := []*cdbm.DpuExtensionService{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if inventory.InventoryPage == nil || inventory.InventoryPage.TotalPages == 0 || (inventory.InventoryPage.CurrentPage == inventory.InventoryPage.TotalPages) {
		for _, dpuExtensionService := range existingDpuExtensionServiceIDMap {
			if !reportedDpuExtensionServiceIDMap[dpuExtensionService.ID] {
				dpuExtensionServicesToDelete = append(dpuExtensionServicesToDelete, dpuExtensionService)
			}
		}
	}

	// Loop through DPU Extension Services for deletion
	for _, dpuExtensionService := range dpuExtensionServicesToDelete {
		slogger := logger.With().Str("DPU Extension Service ID", dpuExtensionService.ID.String()).Logger()

		// Avoid these actions if the object was updated since the inventory was received
		if site.IsTimeWithinStaleInventoryThreshold(dpuExtensionService.Updated) {
			continue
		}

		// If the DPU Extension Service was already being deleted, we can proceed with removing it from the DB
		if dpuExtensionService.Status == cdbm.DpuExtensionServiceStatusDeleting {
			// The DPU Extension Service was being deleted, so delete it from DB
			err := dpuExtensionServiceDAO.Delete(ctx, nil, dpuExtensionService.ID)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to delete DPU Extension Service from DB")
				continue
			}
		} else if !dpuExtensionService.IsMissingOnSite {
			// Mark DPU Extension Service as missing on Site
			_, err := dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{DpuExtensionServiceID: dpuExtensionService.ID, IsMissingOnSite: cutil.GetPtr(true)})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to mark DPU Extension Service as missing on Site in DB")
				continue
			}

			// Create status detail for DPU Extension Service
			_, err = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: dpuExtensionService.ID.String(), Status: cdbm.DpuExtensionServiceStatusError, Message: cutil.GetPtr("DPU Extension Service is missing on Site")})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to create status detail for DPU Extension Service in DB")
				continue
			}
		}
	}

	return nil
}

// NewManageDpuExtensionService returns a new ManageDpuExtensionService activity
func NewManageDpuExtensionService(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageDpuExtensionService {
	return ManageDpuExtensionService{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
