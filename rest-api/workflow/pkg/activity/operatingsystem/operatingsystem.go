// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operatingsystem

import (
	"context"
	"database/sql"
	"errors"
	"time"

	mapset "github.com/deckarep/golang-set/v2"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

const (
	MsgOsImageSynced = "Operating System successfully synced to Site"
)

var (
	// ControllerOsImageStatusMap is a list of valid status for the Controller Os Image
	ControllerOsImageStatusMap = map[corev1.OsImageStatus]bool{
		corev1.OsImageStatus_ImageInProgress:    true,
		corev1.OsImageStatus_ImageUninitialized: true,
		corev1.OsImageStatus_ImageDisabled:      true,
		corev1.OsImageStatus_ImageReady:         true,
		corev1.OsImageStatus_ImageFailed:        true,
	}
)

// ManageOsImage is an activity wrapper for managing Operating System lifecycle for a Site and allows
// injecting DB access
type ManageOsImage struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateOsImagesInDB takes information pushed by Site Agent for a collection of image based OSs associated with the Site and updates the DB
func (mos ManageOsImage) UpdateOsImagesInDB(ctx context.Context, siteID uuid.UUID, osImageInventory *corev1.OsImageInventory) ([]uuid.UUID, error) {
	logger := log.With().Str("Activity", "UpdateOsImagesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mos.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received Os Image inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return nil, err
	}

	if osImageInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil, nil
	}

	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(mos.dbSession)

	existingOssas, _, err := ossaDAO.GetAll(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.OperatingSystemRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get OS Image Site Associations for Site from DB")
		return nil, err
	}

	// Construct a map ID of Operating System Site Association to Operating System.
	//
	// iPXE and Templated iPXE OS associations share the operating_system_site_association
	// table but are reconciled exclusively by the OperatingSystem inventory path
	// (UpdateOperatingSystemsInDB). They never appear in the OS Image inventory, so
	// including them here would repeatedly (and wrongly) flag them as "missing on Site"
	// and flip their status to Error every cycle. Restrict this reconcile to image-based
	// OS associations.
	existingOsImageMap := make(map[string]*cdbm.OperatingSystemSiteAssociation)
	for _, ossa := range existingOssas {
		if ossa.OperatingSystem != nil && cdbm.IsIPXEType(ossa.OperatingSystem.Type) {
			continue
		}
		curossa := ossa
		existingOsImageMap[ossa.OperatingSystemID.String()] = &curossa
	}

	reportedOsImageIDMap := map[uuid.UUID]bool{}

	if osImageInventory.InventoryPage != nil {
		logger.Info().Msgf("Received OS Image inventory page: %d of %d, page size: %d, total count: %d",
			osImageInventory.InventoryPage.CurrentPage, osImageInventory.InventoryPage.TotalPages,
			osImageInventory.InventoryPage.PageSize, osImageInventory.InventoryPage.TotalItems)

		for _, strId := range osImageInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse OS Image ID from inventory page")
				continue
			}
			reportedOsImageIDMap[id] = true
		}
	}

	updatedOperatingSystemMap := map[uuid.UUID]bool{}

	// Iterate through OS Image Inventory and update DB
	for _, controllerOsImage := range osImageInventory.OsImages {
		if controllerOsImage != nil && controllerOsImage.Attributes != nil {

			osImageIDStr := controllerOsImage.Attributes.Id.GetValue()
			slogger := logger.With().Str("OS Image ID", osImageIDStr).Logger()

			ossa, ok := existingOsImageMap[osImageIDStr]
			if !ok {
				slogger.Error().Str("OS Image ID", controllerOsImage.Attributes.Id.Value).Msg("OS Image Site Association does not have a record in DB, possibly created directly on Site")
				continue
			}

			reportedOsImageIDMap[ossa.OperatingSystemID] = true

			// Reset missing flag if necessary
			if ossa.IsMissingOnSite {
				// Update Operating System Site Association missing flag as it is now found on Site
				_, serr := ossaDAO.Update(
					ctx,
					nil,
					cdbm.OperatingSystemSiteAssociationUpdateInput{
						OperatingSystemSiteAssociationID: ossa.ID,
						IsMissingOnSite:                  cutil.GetPtr(false),
					},
				)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to update OS Image Site Association missing flag in DB")
					continue
				}
			}

			if ossa.Status == cdbm.OperatingSystemSiteAssociationStatusDeleting {
				continue
			}

			// Update Operating System Site Association status if necessary
			ossaStatus := cdbm.OperatingSystemSiteAssociationStatusSyncing
			ossaStatusMessage := controllerOsImage.StatusMessage

			ok = ControllerOsImageStatusMap[controllerOsImage.Status]
			if !ok {
				slogger.Error().Str("OS Image ID", controllerOsImage.Attributes.Id.Value).Str("OS Image Status", controllerOsImage.Status.String()).Msg("received unknown OS Image status from Site Agent")
			}

			switch controllerOsImage.Status {
			case corev1.OsImageStatus_ImageInProgress, corev1.OsImageStatus_ImageUninitialized, corev1.OsImageStatus_ImageDisabled:
				ossaStatusMessage = cutil.GetPtr("OS Image is still syncing")
			case corev1.OsImageStatus_ImageReady:
				ossaStatus = cdbm.OperatingSystemSiteAssociationStatusSynced
				ossaStatusMessage = cutil.GetPtr("OS Image is ready to use")
			case corev1.OsImageStatus_ImageFailed:
				ossaStatus = cdbm.OperatingSystemSiteAssociationStatusError
				if ossaStatusMessage == nil || *ossaStatusMessage == "" {
					ossaStatusMessage = cutil.GetPtr("OS Image failed to sync on Site")
				}
			}

			// if determined status is different that current
			// only that case update
			if ossaStatus != ossa.Status {
				serr := mos.updateOperatingSystemSiteAssociationStatusInDB(ctx, nil, ossa.ID, cutil.GetPtr(ossaStatus), ossaStatusMessage)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to update OS Image Site Association status detail in DB")
				}
				updatedOperatingSystemMap[ossa.OperatingSystemID] = true
			}
		}
	}

	// Populate list of ossas that were not found
	ossasToDelete := []*cdbm.OperatingSystemSiteAssociation{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if osImageInventory.InventoryPage == nil || osImageInventory.InventoryPage.TotalPages == 0 || (osImageInventory.InventoryPage.CurrentPage == osImageInventory.InventoryPage.TotalPages) {
		for _, ossa := range existingOsImageMap {
			found := false
			_, found = reportedOsImageIDMap[ossa.OperatingSystemID]
			if !found || ossa.Status == cdbm.OperatingSystemSiteAssociationStatusDeleting {
				// The OS Image was not found in the Os Image Inventory, so add it to list of OS Image to potentially delete
				ossasToDelete = append(ossasToDelete, ossa)
			}
		}
	}

	// Process all Operating Site Associations in DB
	for _, ossa := range ossasToDelete {
		slogger := logger.With().Str("OS Image Site Association ID", ossa.ID.String()).Logger()

		// Operating System was not found on Site
		if ossa.Status == cdbm.OperatingSystemSiteAssociationStatusDeleting {
			// If the OperatingSystemSiteAssociation was being deleted, we can proceed with removing it from the DB
			serr := ossaDAO.Delete(ctx, nil, ossa.ID)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to delete Operating System Site Association from DB")
				continue
			}
			// Trigger re-evaluation of Operating System status (delete if no association exists)
			serr = mos.UpdateOperatingSystemStatusInDB(ctx, ossa.OperatingSystemID)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to trigger Operating System status update in DB")
			}
		} else {
			// Was this created within inventory receipt interval? If so, we may be processing an older inventory
			if site.IsTimeWithinStaleInventoryThreshold(ossa.Created) {
				continue
			}

			// Set isMissingOnSite flag to true and update status, user can decide on deletion
			_, serr := ossaDAO.Update(
				ctx,
				nil,
				cdbm.OperatingSystemSiteAssociationUpdateInput{
					OperatingSystemSiteAssociationID: ossa.ID,
					IsMissingOnSite:                  cutil.GetPtr(true),
				},
			)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB for Operating System Site Association")
				continue
			}

			serr = mos.updateOperatingSystemSiteAssociationStatusInDB(ctx, nil, ossa.ID, cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusError), cutil.GetPtr("Operating System is missing on Site"))
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update Operating System Site Association status detail in DB")
			}

			updatedOperatingSystemMap[ossa.OperatingSystemID] = true
		}
	}

	updatedOsIDs := []uuid.UUID{}
	for osID := range updatedOperatingSystemMap {
		updatedOsIDs = append(updatedOsIDs, osID)
	}

	return updatedOsIDs, nil
}

// updateOperatingSystemSiteAssociationStatusInDB is helper function to write OperatingSystemSiteAssociation updates to DB
func (mos ManageOsImage) updateOperatingSystemSiteAssociationStatusInDB(ctx context.Context, tx *cdb.Tx, ossaID uuid.UUID, status *string, statusMessage *string) error {
	if status != nil {
		ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(mos.dbSession)

		_, err := ossaDAO.Update(
			ctx,
			tx,
			cdbm.OperatingSystemSiteAssociationUpdateInput{
				OperatingSystemSiteAssociationID: ossaID,
				Status:                           status,
			},
		)
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mos.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ossaID.String(), Status: *status, Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// UpdateOperatingSystemStatusInDB is helper function to write Operating System updates to DB
func (mos ManageOsImage) UpdateOperatingSystemStatusInDB(ctx context.Context, osID uuid.UUID) error {
	logger := log.With().Str("Activity", "UpdateOperatingSystemStatusInDB").Str("Operating System ID", osID.String()).Logger()

	logger.Info().Msg("starting activity")

	osDAO := cdbm.NewOperatingSystemDAO(mos.dbSession)

	os, err := osDAO.GetByID(ctx, nil, osID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received request for unknown or deleted Operating System")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Operating System from DB")
		}
		return nil
	}

	logger.Info().Msg("retrieved Operating System from DB")

	var osStatus *string
	var osMessage *string

	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(mos.dbSession)
	ossas, ossaTotal, err := ossaDAO.GetAll(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationFilterInput{
			OperatingSystemIDs: []uuid.UUID{osID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get Operating System Site Associations from DB for Operating System")
		return err
	}

	// Operating System is in deleting state
	if os.Status == cdbm.OperatingSystemStatusDeleting {
		if ossaTotal == 0 {
			// Start a db tx
			tx, err := cdb.BeginTx(ctx, mos.dbSession, &sql.TxOptions{})
			if err != nil {
				logger.Error().Err(err).Msg("failed to start transaction")
				return err
			}

			// No more associations left, we can delete the Operating System
			serr := osDAO.Delete(ctx, tx, osID)
			if serr != nil {
				logger.Error().Err(serr).Msg("failed to delete Operating System from DB")
				terr := tx.Rollback()
				if terr != nil {
					logger.Error().Err(terr).Msg("failed to rollback transaction")
				}
				return serr
			}

			// Commit transaction
			err = tx.Commit()
			if err != nil {
				logger.Error().Err(err).Msg("error committing transaction to DB")
				return err
			}
		}

		// One or more associations left to delete from Sites
		return nil
	}

	if ossaTotal == 0 {
		if os.Status == cdbm.OperatingSystemStatusReady {
			return nil
		}
		osStatus = cutil.GetPtr(cdbm.OperatingSystemStatusReady)
		osMessage = cutil.GetPtr("Operating System successfully synced to all Sites")
	} else {
		statusCountMap := map[string]int{}
		for _, dbossa := range ossas {
			statusCountMap[dbossa.Status]++
		}

		if statusCountMap[cdbm.OperatingSystemSiteAssociationStatusError] > 0 {
			if os.Status == cdbm.OperatingSystemStatusError {
				return nil
			}
			osStatus = cutil.GetPtr(cdbm.OperatingSystemStatusError)
			osMessage = cutil.GetPtr("Failed to sync Operating System to one or more Sites")
		} else if statusCountMap[cdbm.OperatingSystemSiteAssociationStatusSyncing] > 0 {
			if os.Status == cdbm.OperatingSystemStatusSyncing {
				return nil
			}
			osStatus = cutil.GetPtr(cdbm.OperatingSystemStatusSyncing)
			osMessage = cutil.GetPtr("Operating System syncing to one or more Sites")
		} else {
			if os.Status == cdbm.OperatingSystemStatusReady {
				return nil
			}
			osStatus = cutil.GetPtr(cdbm.OperatingSystemStatusReady)
			osMessage = cutil.GetPtr("Operating System successfully synced to all Sites")
		}
	}

	// Update status
	_, err = osDAO.Update(
		ctx,
		nil,
		cdbm.OperatingSystemUpdateInput{
			OperatingSystemId: osID,
			Status:            osStatus,
		},
	)
	if err != nil {
		return err
	}

	statusDetailDAO := cdbm.NewStatusDetailDAO(mos.dbSession)
	_, err = statusDetailDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: osID.String(), Status: *osStatus, Message: osMessage})
	if err != nil {
		return err
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// UpdateOperatingSystemsInDB reconciles the operating_system table for a Site based on Operating Systems reported from Site
func (mos ManageOsImage) UpdateOperatingSystemsInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.OperatingSystemInventory) error {
	logger := log.With().Str("Activity", "UpdateOperatingSystemsInDB").Str("Site ID", siteID.String()).Logger()
	logger.Info().Msg("Starting activity")

	if inventory == nil {
		return errors.New("UpdateOperatingSystemsInDB called with nil inventory")
	}

	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("Received failed inventory status from Site Agent, skipping")
		return nil
	}

	stDAO := cdbm.NewSiteDAO(mos.dbSession)
	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("Received inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("Failed to retrieve Site from DB")
		}
		return err
	}

	// Resolve the reporting Site's provider. It is used as the owner for OSes
	// reported without tenant_organization_id; OSes that carry a tenant org are
	// attributed to the matching REST tenant below.
	logger.Debug().Str("InfrastructureProviderID", site.InfrastructureProviderID.String()).Msg("Resolved Infrastructure Provider from Site")

	osDAO := cdbm.NewOperatingSystemDAO(mos.dbSession)
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(mos.dbSession)
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(mos.dbSession)

	// Collect the UUIDs of all reported OS records (active only — the new Find APIs do not
	// return deleted records). Site and REST share the same UUID as PK.
	reportedOSIDs := mapset.NewSet[uuid.UUID]()
	for _, reportedOS := range inventory.GetOperatingSystems() {
		if reportedOS == nil {
			logger.Error().Msg("Received nil OS record in inventory, skipping")
			continue
		}

		controllerOSID := reportedOS.GetId().GetValue()
		if controllerOSID == "" {
			logger.Error().Msg("Received OS record with empty ID, skipping")
			continue
		}

		reportedOSID, parseErr := uuid.Parse(controllerOSID)
		if parseErr != nil {
			logger.Error().Err(parseErr).Str("ControllerOperatingSystemID", controllerOSID).Msg("Received OS record with invalid UUID, skipping")
			continue
		}
		reportedOSIDs.Add(reportedOSID)
	}

	// Fetch DB records matching the reported IDs (including soft-deleted so we can detect
	// the case where REST already deleted an OS that Site still reports active).
	existingOSes, _, err := osDAO.GetAll(ctx, nil, cdbm.OperatingSystemFilterInput{
		OperatingSystemIds: reportedOSIDs.ToSlice(),
		IncludeDeleted:     true,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)

	if err != nil {
		logger.Error().Err(err).Msg("Failed to get Operating Systems from DB")
		return err
	}

	existingOSByID := map[uuid.UUID]*cdbm.OperatingSystem{}
	for i := range existingOSes {
		existingOSByID[existingOSes[i].ID] = &existingOSes[i]
	}

	// Track multi-site (REST source-of-truth) OS IDs that need aggregate status
	// recomputation from their per-site controller states.
	multiSiteOSIDs := map[uuid.UUID]struct{}{}

	// Cache org -> tenant id resolutions for this inventory cycle so several OSes
	// owned by the same tenant do not each trigger a tenant lookup.
	tenantDAO := cdbm.NewTenantDAO(mos.dbSession)
	tenantOrgToID := map[string]*uuid.UUID{}

	// Create or update OSes based on the Site inventory.
	for _, reportedOS := range inventory.GetOperatingSystems() {
		if reportedOS == nil || reportedOS.GetId().GetValue() == "" {
			continue
		}

		reportedOSID, parseErr := uuid.Parse(reportedOS.GetId().GetValue())
		if parseErr != nil {
			continue
		}

		slogger := logger.With().Str("ControllerOperatingSystemID", reportedOSID.String()).Logger()

		// A missing or malformed Updated yields the zero time, so the
		// coreUpdated.After(existingOS.Updated) check below stays false and no
		// timestamp-driven definition update is performed for this OS this cycle
		// (other reconciliation reasons still apply). Log it so bad input from the
		// Site is visible rather than silently suppressing updates.
		coreUpdated, tsErr := time.Parse(time.RFC3339, reportedOS.Updated)
		if tsErr != nil {
			slogger.Warn().Err(tsErr).Str("Updated", reportedOS.Updated).Msg("Operating System has missing/invalid Updated timestamp from Site; skipping timestamp-based definition update")
		}

		ipxeTemplateParams := []cdbm.OperatingSystemIpxeParameter{}
		for _, param := range reportedOS.IpxeTemplateParameters {
			ipxeTemplateParam := cdbm.OperatingSystemIpxeParameter{}
			ipxeTemplateParam.FromProto(param)
			ipxeTemplateParams = append(ipxeTemplateParams, ipxeTemplateParam)
		}

		ipxeTemplateArtifacts := []cdbm.OperatingSystemIpxeArtifact{}
		for _, artifact := range reportedOS.IpxeTemplateArtifacts {
			ipxeTemplateArtifact := cdbm.OperatingSystemIpxeArtifact{}
			ipxeTemplateArtifact.FromProto(artifact)
			ipxeTemplateArtifacts = append(ipxeTemplateArtifacts, ipxeTemplateArtifact)
		}

		osType := cdbm.OperatingSystemTypeFromProtoMap[reportedOS.Type]
		if osType == "" {
			slogger.Error().Str("Type", reportedOS.Type.String()).Msg("Received unknown OS type from Site, skipping")
			continue
		}

		// Ownership is derived from nico-core's optional tenant_organization_id:
		// when present the OS is tenant-owned (org + resolved tenant id); when absent
		// it is provider-owned (the reporting Site's org + infrastructure provider).
		// These values feed both the create path and the single-site update/backfill.
		//
		// Strict attribution: if tenant_organization_id is set but does not resolve to
		// a known REST tenant, ownership cannot be attributed, so the OS is neither
		// imported nor updated this cycle (rather than being stored orphaned and thus
		// invisible to every caller). It is retried on the next inventory cycle once
		// the tenant exists.
		ownerOrg := site.Org
		ownerProviderID := &site.InfrastructureProviderID
		var ownerTenantID *uuid.UUID
		if tenantOrg := reportedOS.GetTenantOrganizationId(); tenantOrg != "" {
			ownerOrg = tenantOrg
			ownerProviderID = nil
			tenantID, cached := tenantOrgToID[tenantOrg]
			if !cached {
				tenants, _, terr := tenantDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{
					Orgs: []string{tenantOrg},
				}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				switch {
				case terr != nil:
					// Transient lookup failure: do not cache, so later Operating
					// Systems for this org retry resolution this cycle.
					slogger.Error().Err(terr).Str("TenantOrg", tenantOrg).Msg("Failed to resolve tenant for Operating System; skipping this cycle")
				case len(tenants) == 0:
					slogger.Error().Str("TenantOrg", tenantOrg).Msg("No REST tenant matches Operating System's tenant_organization_id; skipping OS (cannot attribute ownership)")
					tenantOrgToID[tenantOrg] = tenantID
				default:
					if len(tenants) > 1 {
						slogger.Warn().Str("TenantOrg", tenantOrg).Msg("Multiple tenants found for Operating System org; using first")
					}
					tenantID = cutil.GetPtr(tenants[0].ID)
					tenantOrgToID[tenantOrg] = tenantID
				}
			}
			if tenantID == nil {
				// tenant_organization_id is set but unknown to REST: skip rather than
				// create/update an unattributable (orphaned) Operating System.
				continue
			}
			ownerTenantID = tenantID
		}

		existingOS, found := existingOSByID[reportedOSID]
		if !found {
			// Templated iPXE OS: require a non-empty template reference that is
			// available at this site before creating the OS record. Skip silently
			// if it is missing, invalid, or not associated with the Site.
			if osType == cdbm.OperatingSystemTypeTemplatedIPXE {
				if reportedOS.IpxeTemplateId.GetValue() == "" {
					slogger.Warn().Msg("Templated iPXE Operating System has no iPXE template reference, skipping")
					continue
				}
				ipxeTemplateID, serr := uuid.Parse(reportedOS.IpxeTemplateId.GetValue())
				if serr != nil {
					slogger.Error().Err(serr).Str("IpxeTemplateID", reportedOS.IpxeTemplateId.GetValue()).Msg("Invalid iPXE template UUID in Operating System, skipping")
					continue
				}

				_, serr = itsaDAO.GetByIpxeTemplateIDAndSiteID(ctx, nil, ipxeTemplateID, siteID, nil)
				if serr != nil {
					if errors.Is(serr, cdb.ErrDoesNotExist) {
						slogger.Warn().Str("IpxeTemplateID", ipxeTemplateID.String()).Msg("iPXE Template Association does not exist for Site, skipping")
						continue
					}
					slogger.Error().Err(serr).Msg("Failed to retrieve IpxeTemplateSiteAssociation, DB error")
					continue
				}
			}

			// New OS discovered from Site. Ownership follows nico-core's optional
			// tenant_organization_id (resolved above): tenant-owned when present,
			// otherwise provider-owned (the reporting Site's provider). A newly
			// discovered OS starts with exactly one associated Site, so it is
			// bidirectionally synced (core-driven) until more sites are added.

			// Create site association linking the OS to the reporting site.
			ossaStatus := cdbm.OperatingSystemSiteAssociationStatusFromProtoMap[reportedOS.Status]
			if ossaStatus == "" {
				slogger.Warn().Str("Status", reportedOS.Status.String()).Msg("Received unknown status from Site, using `Syncing` as default")
				ossaStatus = cdbm.OperatingSystemSiteAssociationStatusSyncing
			}

			// Build the create input: definition fields come from the reported proto
			// (FromProto); ID and ownership come from the Site sync context.
			osInput := cdbm.OperatingSystemCreateInput{}
			osInput.FromProto(reportedOS)
			osInput.ID = reportedOSID
			osInput.Org = ownerOrg
			osInput.InfrastructureProviderID = ownerProviderID
			osInput.TenantID = ownerTenantID

			// The OS definition, the (optional) inactive correction, and the per-site
			// association are dependent writes: commit them together so a later
			// failure cannot leave a partially-created OS.
			txErr := cdb.WithTx(ctx, mos.dbSession, func(tx *cdb.Tx) error {
				if _, serr := osDAO.Create(ctx, tx, osInput); serr != nil {
					slogger.Error().Err(serr).Msg("Failed to create Operating System, DB error")
					return serr
				}

				if !reportedOS.IsActive {
					// TODO: Allow creation of inactive OSes
					if _, serr := osDAO.Update(ctx, tx, cdbm.OperatingSystemUpdateInput{
						OperatingSystemId: reportedOSID,
						IsActive:          cutil.GetPtr(false),
					}); serr != nil {
						slogger.Error().Err(serr).Msg("Failed to set Operating System to inactive on creation")
						return serr
					}
				}

				if _, serr := ossaDAO.Create(ctx, tx, cdbm.OperatingSystemSiteAssociationCreateInput{
					OperatingSystemID: reportedOSID,
					SiteID:            siteID,
					Status:            ossaStatus,
				}); serr != nil {
					slogger.Error().Err(serr).Msg("Failed to create site association for new OS")
					return serr
				}

				return nil
			})
			if txErr != nil {
				// The inner DAO failures log their own specifics; this also
				// surfaces begin/commit failures, which WithTx returns but no
				// inner handler logs, so they aren't swallowed silently here.
				slogger.Error().Err(txErr).Msg("Failed to create Operating System in transaction, skipping")
				continue
			}

			// Newly-created OS: definition and per-site association have just been
			// written with the reported state. Skip the existing-OS update path
			// below (it dereferences existingOS which is nil here); a brand new OS
			// has a single associated Site, so it is not a multi-site (REST-owned)
			// record and needs no aggregate-status recomputation this cycle.
			continue
		}

		// REST layer has already soft-deleted this OS (user-initiated)
		// Do not restore it even if Site still reports it as active (the delete push to Site may be in-flight)
		if existingOS.Deleted != nil {
			continue
		}

		// Update or create the per-site association for every OS type. The
		// definition update below is gated on the association count (computed after
		// this association is ensured): a single associated Site is bidirectional
		// (core-driven), while multiple associated Sites make REST the source of
		// truth so we only record the Site's controller state.
		controllerState := cdbm.OperatingSystemStatusFromProtoMap[reportedOS.Status]
		if controllerState == "" {
			slogger.Warn().Str("Status", reportedOS.Status.String()).Msg("Received unknown status from Site, using `Syncing` as default")
			controllerState = cdbm.OperatingSystemStatusSyncing
		}

		ossaStatus := cdbm.OperatingSystemSiteAssociationStatusFromProtoMap[reportedOS.Status]
		if ossaStatus == "" {
			slogger.Warn().Str("Status", reportedOS.Status.String()).Msg("Received unknown status from Site, using `Syncing` as default")
			ossaStatus = cdbm.OperatingSystemSiteAssociationStatusSyncing
		}

		ossa, serr := ossaDAO.GetByOperatingSystemIDAndSiteID(ctx, nil, reportedOSID, siteID, nil)
		if serr != nil {
			if !errors.Is(serr, cdb.ErrDoesNotExist) {
				slogger.Error().Err(serr).Msg("Failed to retrieve Operating System Site Association, DB error")
				continue
			}

			// Operating System Site Association is missing, create it
			_, serr := ossaDAO.Create(ctx, nil, cdbm.OperatingSystemSiteAssociationCreateInput{
				OperatingSystemID: reportedOSID,
				SiteID:            siteID,
				Status:            ossaStatus,
				ControllerState:   &controllerState,
			})
			if serr != nil {
				slogger.Error().Err(serr).Msg("Failed to create Operating System Site Association")
				continue
			}
		} else {
			// Update existing Operating System Site Association
			_, uerr := ossaDAO.Update(ctx, nil, cdbm.OperatingSystemSiteAssociationUpdateInput{
				OperatingSystemSiteAssociationID: ossa.ID,
				Status:                           &ossaStatus,
				ControllerState:                  &controllerState,
			})
			if uerr != nil {
				slogger.Error().Err(uerr).Msg("Failed to update Operating System Site Association")
				continue
			}
		}

		// Decide the source of truth from the association count (now that this
		// Site's association is ensured): exactly one associated Site means the OS
		// is bidirectionally synced and nico-core drives the definition; two or more
		// make REST the source of truth, so we only recompute aggregate status.
		osAssocs, _, cerr := ossaDAO.GetAll(ctx, nil, cdbm.OperatingSystemSiteAssociationFilterInput{
			OperatingSystemIDs: []uuid.UUID{reportedOSID},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if cerr != nil {
			slogger.Error().Err(cerr).Msg("Failed to count Operating System Site Associations, skipping definition update")
			continue
		}
		isSingleSite := len(osAssocs) == 1
		if !isSingleSite {
			multiSiteOSIDs[reportedOSID] = struct{}{}
		}

		// Single-site OS: nico-core is the source of truth for the definition.
		// Backfill only fills missing ownership values (it never reassigns an
		// existing owner) so a provider-owned record cannot silently be re-homed.
		needsOwnershipBackfill := isSingleSite &&
			((existingOS.Org == "" && ownerOrg != "") ||
				(ownerProviderID != nil && existingOS.InfrastructureProviderID == nil) ||
				(ownerTenantID != nil && existingOS.TenantID == nil))
		needsIsActiveCorrection := isSingleSite && existingOS.IsActive != reportedOS.IsActive

		if isSingleSite && (coreUpdated.After(existingOS.Updated) || needsOwnershipBackfill || needsIsActiveCorrection) {
			controllerState := cdbm.OperatingSystemStatusFromProtoMap[reportedOS.Status]
			if controllerState == "" {
				slogger.Warn().Str("Status", reportedOS.Status.String()).Msg("Received unknown status from Site, using `Syncing` as default")
				controllerState = cdbm.OperatingSystemStatusSyncing
			}

			// Templated iPXE OS: require a non-empty template reference that is
			// available at this site before overwriting the OS record. Skip the
			// update if it is missing, invalid, or not associated with the Site.
			// Other OS types carry no template reference.
			var ipxeTemplateID *string
			if osType == cdbm.OperatingSystemTypeTemplatedIPXE {
				if reportedOS.IpxeTemplateId.GetValue() == "" {
					slogger.Warn().Msg("Templated iPXE Operating System has no iPXE template reference, skipping update")
					continue
				}
				parsedTemplateID, serr := uuid.Parse(reportedOS.IpxeTemplateId.GetValue())
				if serr != nil {
					slogger.Error().Err(serr).Str("IpxeTemplateID", reportedOS.IpxeTemplateId.GetValue()).Msg("Invalid iPXE template UUID in Operating System, skipping update")
					continue
				}
				if _, serr = itsaDAO.GetByIpxeTemplateIDAndSiteID(ctx, nil, parsedTemplateID, siteID, nil); serr != nil {
					if errors.Is(serr, cdb.ErrDoesNotExist) {
						slogger.Warn().Str("IpxeTemplateID", parsedTemplateID.String()).Msg("iPXE Template Association does not exist for Site, skipping update")
						continue
					}
					slogger.Error().Err(serr).Msg("Failed to retrieve IpxeTemplateSiteAssociation, DB error")
					continue
				}
				ipxeTemplateID = cutil.GetPtr(reportedOS.IpxeTemplateId.GetValue())
			}

			updateInput := cdbm.OperatingSystemUpdateInput{
				OperatingSystemId:        existingOS.ID,
				Name:                     &reportedOS.Name,
				Org:                      &ownerOrg,
				TenantID:                 ownerTenantID,
				InfrastructureProviderID: ownerProviderID,
				OsType:                   &osType,
				Description:              reportedOS.Description,
				UserData:                 reportedOS.UserData,
				IpxeScript:               reportedOS.IpxeScript,
				AllowOverride:            &reportedOS.AllowOverride,
				PhoneHomeEnabled:         &reportedOS.PhoneHomeEnabled,
				IsActive:                 &reportedOS.IsActive,
				IpxeTemplateId:           ipxeTemplateID,
				IpxeTemplateParameters:   &ipxeTemplateParams,
				IpxeTemplateArtifacts:    &ipxeTemplateArtifacts,
				IpxeOSHash:               reportedOS.IpxeTemplateDefinitionHash,
				Status:                   &controllerState,
			}
			// Ownership is mutually exclusive: exactly one of TenantID /
			// InfrastructureProviderID is populated. Update only writes the active
			// owner column (the inactive one is nil and thus left unchanged), so
			// when ownership flips we must explicitly clear the now-inactive column
			// to avoid leaving both populated. Update + Clear run in one transaction.
			var ownerClearInput *cdbm.OperatingSystemClearInput
			switch {
			case ownerTenantID != nil && existingOS.InfrastructureProviderID != nil:
				ownerClearInput = &cdbm.OperatingSystemClearInput{OperatingSystemId: existingOS.ID, InfrastructureProviderID: true}
			case ownerProviderID != nil && existingOS.TenantID != nil:
				ownerClearInput = &cdbm.OperatingSystemClearInput{OperatingSystemId: existingOS.ID, TenantID: true}
			}

			uerr := cdb.WithTx(ctx, mos.dbSession, func(tx *cdb.Tx) error {
				if _, serr := osDAO.Update(ctx, tx, updateInput); serr != nil {
					return serr
				}
				if ownerClearInput != nil {
					if _, serr := osDAO.Clear(ctx, tx, *ownerClearInput); serr != nil {
						return serr
					}
				}
				return nil
			})
			if uerr != nil {
				slogger.Error().Err(uerr).Msg("Failed to update Operating System, DB error")
				continue
			}
		}
	}

	// Deletion propagation: Site's Find APIs return only active records, so any iPXE OS
	// in our DB that is NOT in this inventory was deleted in nico-core. Soft-delete it here.
	// Image-based OSes are not managed by this inventory, so we restrict to iPXE types only.
	// Only single-site OSes (exactly one associated Site) are bidirectionally synced and
	// therefore subject to deletion-by-absence; multi-site OSes are REST-owned and must
	// not be deleted from Site inventory, and raw iPXE OSes have no associations at all.
	//
	// Inventory may be paged: each page carries only a subset in OperatingSystems but the
	// full reported ID set in InventoryPage.ItemIds. Deletion must therefore run against
	// the complete reported set, and only once per sweep — on the final page — so that an
	// earlier page does not prematurely soft-delete an OS that appears on a later page.
	page := inventory.GetInventoryPage()
	isFinalPage := page == nil || page.TotalPages == 0 || page.CurrentPage == page.TotalPages
	if isFinalPage {
		// Build the complete set of reported OS IDs. When paging is in use the full set
		// lives in InventoryPage.ItemIds; otherwise the single message's OperatingSystems
		// already is the complete set (captured above in reportedOSIDs).
		deletionReportedIDs := reportedOSIDs
		if page != nil && len(page.ItemIds) > 0 {
			deletionReportedIDs = mapset.NewSet[uuid.UUID]()
			for _, strID := range page.ItemIds {
				id, perr := uuid.Parse(strID)
				if perr != nil {
					logger.Error().Err(perr).Str("ID", strID).Msg("Failed to parse OS ID from inventory page, skipping")
					continue
				}
				deletionReportedIDs.Add(id)
			}
		}

		// Scope deletion to the reporting Site: only OSes associated with this Site
		// are candidates, so an OS that lives at a different Site is not soft-deleted
		// just because it is absent from this Site's inventory.
		siteOssas, _, derr := ossaDAO.GetAll(ctx, nil, cdbm.OperatingSystemSiteAssociationFilterInput{
			SiteIDs: []uuid.UUID{siteID},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("Failed to fetch Operating System Site Associations for deletion reconciliation")
			return derr
		}
		candidateOSIDs := make([]uuid.UUID, 0, len(siteOssas))
		// Deletion below requires a single association, so this Site's association is the only
		// one and its creation time is when the OS became known to the Site.
		assocCreatedByOS := make(map[uuid.UUID]time.Time, len(siteOssas))
		for _, ossa := range siteOssas {
			candidateOSIDs = append(candidateOSIDs, ossa.OperatingSystemID)
			assocCreatedByOS[ossa.OperatingSystemID] = ossa.Created
		}

		if len(candidateOSIDs) > 0 {
			// Count total associations per candidate OS across all Sites: only OSes
			// with a single associated Site are bidirectionally synced (core-driven)
			// and thus eligible for reconciliation-by-absence.
			allAssocs, _, derr := ossaDAO.GetAll(ctx, nil, cdbm.OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: candidateOSIDs,
			}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("Failed to count Operating System Site Associations for deletion reconciliation")
				return derr
			}
			assocCountByOS := map[uuid.UUID]int{}
			for _, ossa := range allAssocs {
				assocCountByOS[ossa.OperatingSystemID]++
			}

			// Restrict to iPXE-type OSes (Image OSes are not managed by this inventory).
			candidateOSes, _, derr := osDAO.GetAll(ctx, nil, cdbm.OperatingSystemFilterInput{
				OperatingSystemIds: candidateOSIDs,
				OsTypes:            []string{cdbm.OperatingSystemTypeIPXE, cdbm.OperatingSystemTypeTemplatedIPXE},
			}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("Failed to fetch iPXE Operating Systems from DB for deletion reconciliation")
				return derr
			}

			for _, ipxeOS := range candidateOSes {
				// Only single-site (bidirectional) OSes are subject to deletion-by-absence.
				if assocCountByOS[ipxeOS.ID] != 1 {
					continue
				}

				slogger := logger.With().Str("OperatingSystemID", ipxeOS.ID.String()).Logger()

				if !deletionReportedIDs.Contains(ipxeOS.ID) {
					// An OS associated to this Site after the inventory was collected is absent
					// from it for that reason alone. A later inventory reads an already-deleted
					// OS as a user decision and will not restore it, so defer to the next run.
					if site.IsTimeWithinStaleInventoryThreshold(assocCreatedByOS[ipxeOS.ID]) {
						slogger.Info().Msg("Not soft-deleting iPXE OS yet because its Site association is newer than the inventory interval")

						continue
					}
					slogger.Info().Msg("Soft-deleting single-site iPXE OS absent from Site inventory")
					serr := osDAO.Delete(ctx, nil, ipxeOS.ID)
					if serr != nil {
						slogger.Error().Err(serr).Msg("Failed to soft-delete OS, DB error")
						continue
					}
				}
			}
		}
	}

	// Aggregate status for multi-site (REST source-of-truth) OSes from their per-site
	// core statuses. Rule: if all Site Associations have `Ready` status then the
	// Operating System is `Ready`. Otherwise, it is `Syncing`.
	if len(multiSiteOSIDs) > 0 {
		ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(mos.dbSession)
		for osID := range multiSiteOSIDs {
			slogger := logger.With().Str("OperatingSystemID", osID.String()).Logger()

			ossas, _, serr := ossaDAO.GetAll(ctx, nil, cdbm.OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: []uuid.UUID{osID},
			}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)

			if serr != nil {
				slogger.Error().Err(serr).Msg("Failed to fetch Site Associations to determine Operating System status, DB error")
				continue
			}

			allReady := true
			for _, ossa := range ossas {
				if ossa.Status != cdbm.OperatingSystemSiteAssociationStatusSynced {
					allReady = false
					break
				}
			}

			aggregatedStatus := cdbm.OperatingSystemStatusSyncing
			if allReady && len(ossas) > 0 {
				aggregatedStatus = cdbm.OperatingSystemStatusReady
			}

			_, serr = osDAO.Update(ctx, nil, cdbm.OperatingSystemUpdateInput{
				OperatingSystemId: osID,
				Status:            &aggregatedStatus,
			})
			if serr != nil {
				slogger.Error().Err(serr).Msg("Failed to update aggregate OS status, DB error")
			}
		}
	}

	logger.Info().Msg("Completed activity")

	return nil
}

// NewManageOsImage returns a new ManageOsImage activity
func NewManageOsImage(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageOsImage {
	return ManageOsImage{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
