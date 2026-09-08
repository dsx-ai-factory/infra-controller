// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcpeering

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ManageVpcPeering is an activity wrapper for managing VPC Peering lifecycle
// that allows injecting DB access
type ManageVpcPeering struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateVpcPeeringsInDB is a Temporal activity that takes a collection of
// VpcPeering data pushed by Site Agent and updates the DB
func (mvp ManageVpcPeering) UpdateVpcPeeringsInDB(
	ctx context.Context,
	siteID uuid.UUID,
	vpcPeeringInventory *corev1.VPCPeeringInventory,
) error {
	logger := log.With().Str("Activity", "UpdateVpcPeeringsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if vpcPeeringInventory == nil {
		logger.Error().Msg("UpdateVpcPeeringsInDB called with nil inventory")
		return errors.New("UpdateVpcPeeringsInDB called with nil inventory")
	}

	// Check if Site exists in DB
	stDAO := cdbm.NewSiteDAO(mvp.dbSession)
	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received VPC Peering inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	// Check if inventory status is correct
	if vpcPeeringInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	vpcPeeringDAO := cdbm.NewVpcPeeringDAO(mvp.dbSession)
	existingVpcPeerings, _, err := vpcPeeringDAO.GetAll(ctx, nil, cdbm.VpcPeeringFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get VPC Peeringes for site from DB")
		return err
	}

	// Map of existing VPC Peerings in Cloud DB
	existingVpcPeeringIDMap := make(map[string]*cdbm.VpcPeering)
	for _, vpcPeering := range existingVpcPeerings {
		curVpcPeering := vpcPeering
		existingVpcPeeringIDMap[vpcPeering.ID.String()] = &curVpcPeering
	}

	// Map of VPC Peerings reported by Site Agent
	reportedVpcPeeringIDMap := map[uuid.UUID]bool{}
	// If inventory paging is enabled, we can get this list of item IDs from the inventory page's ItemIds field;
	// otherwise, we'll have to iterate through all VPC Peerings in the inventory later.
	if vpcPeeringInventory.InventoryPage != nil {
		logger.Info().Msgf("received VPC Peering inventory page: %d of %d, page size: %d, total count: %d",
			vpcPeeringInventory.InventoryPage.CurrentPage, vpcPeeringInventory.InventoryPage.TotalPages,
			vpcPeeringInventory.InventoryPage.PageSize, vpcPeeringInventory.InventoryPage.TotalItems)

		for _, strId := range vpcPeeringInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse VPC Peering ID from inventory page")
				continue
			}
			reportedVpcPeeringIDMap[id] = true
		}
	}

	// Iterate through VpcPeering Inventory and update DB
	for _, controllerVpcPeering := range vpcPeeringInventory.VpcPeerings {
		if controllerVpcPeering == nil || controllerVpcPeering.GetId().GetValue() == "" {
			logger.Error().Msg("received VPC Peering inventory entry with missing controller ID, skipping")
			continue
		}

		controllerVpcPeeringID := controllerVpcPeering.GetId().GetValue()
		slogger := logger.With().Str("VPC Peering Controller ID", controllerVpcPeeringID).Logger()
		vpcPeering := existingVpcPeeringIDMap[controllerVpcPeeringID]

		if vpcPeering == nil {
			vpcPeering = mvp.createOrUpdateVpcPeeringFromSite(ctx, site, controllerVpcPeering)
			if vpcPeering == nil {
				continue
			}

			existingVpcPeeringIDMap[vpcPeering.ID.String()] = vpcPeering
			logger.Info().Str("VPC Peering ID", vpcPeering.ID.String()).Msg("created or undeleted VPC Peering from Site inventory")
		}

		// In the case inventory paging is not enabled, we build reportedVpcPeeringIdMap here.
		// This is redundant if paging is used, but isn't expensive.
		reportedVpcPeeringIDMap[vpcPeering.ID] = true

		// If VPC Peering is not in Deleting state, then update status to Ready
		if vpcPeering.Status != cdbm.VpcPeeringStatusDeleting && vpcPeering.Status != cdbm.VpcPeeringStatusReady {
			err = mvp.updateVpcPeeringStatusInDB(ctx, nil, vpcPeering.ID, cwutil.GetPtr(cdbm.VpcPeeringStatusReady), cwutil.GetPtr("VPC Peering has been re-detected on Site"))
			if err != nil {
				slogger.Error().Err(err).Msg("failed to update VPC Peering status detail in DB")
			}
		}

	}

	// Delete VPC Peerings that are not in the inventory. If inventory paging is enabled, we only need to do this once and we do it on the last page
	if vpcPeeringInventory.InventoryPage == nil || vpcPeeringInventory.InventoryPage.TotalPages == 0 || (vpcPeeringInventory.InventoryPage.CurrentPage == vpcPeeringInventory.InventoryPage.TotalPages) {
		for _, vpcPeering := range existingVpcPeeringIDMap {
			slogger := logger.With().Str("VPC Peering ID", vpcPeering.ID.String()).Logger()
			slogger.Info().Msg("checking for deletion")
			_, found := reportedVpcPeeringIDMap[vpcPeering.ID]
			if !found {

				// The Vpc Peering was not found in the VPC Peering Inventory,
				// so we should delete it, but we might be processing an older
				// inventory, so make sure the object has existed for at least as
				// long as our inventory interval with a little buffer to make
				// sure we aren't in lock-step.
				if site.IsTimeWithinStaleInventoryThreshold(vpcPeering.Created) {
					slogger.Info().Msg("not going to delete yet because VPC Peering is newer than the inventory interval")
					continue
				}

				slogger.Info().Msg("going to delete")

				serr := vpcPeeringDAO.Delete(ctx, nil, vpcPeering.ID)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to delete VPC Peering from DB")
				}
			}
		}
	}

	return nil
}

// createOrUpdateVpcPeeringFromSite creates a REST VPC Peering from Site inventory,
// or undeletes a matching soft-deleted row. Returns nil when skipped or on failure.
func (mvp ManageVpcPeering) createOrUpdateVpcPeeringFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerVpcPeering *corev1.VpcPeering,
) *cdbm.VpcPeering {
	logger := log.With().
		Str("Activity", "UpdateVpcPeeringsInDB").
		Str("Site ID", site.ID.String()).
		Str("VPC Peering Controller ID", controllerVpcPeering.GetId().GetValue()).
		Logger()

	controllerVpcPeeringID, err := uuid.Parse(controllerVpcPeering.GetId().GetValue())
	if err != nil {
		logger.Warn().Msgf("unable to create VPC Peering found on Site: failed to parse VPC Peering Controller ID, not a valid UUID %s", controllerVpcPeering.GetId().GetValue())
		return nil
	}

	reportedVpcPeering := new(cdbm.VpcPeering)
	reportedVpcPeering.FromProto(controllerVpcPeering)
	if reportedVpcPeering.Vpc1ID == uuid.Nil {
		logger.Warn().Msg("unable to create VPC Peering found on Site: VPC Peering on Site is reporting empty VPC ID")
		return nil
	}
	if reportedVpcPeering.Vpc2ID == uuid.Nil {
		logger.Warn().Msg("unable to create VPC Peering found on Site: VPC Peering on Site is reporting empty peer VPC ID")
		return nil
	}
	if reportedVpcPeering.Vpc1ID == reportedVpcPeering.Vpc2ID {
		logger.Warn().Msg("unable to create VPC Peering found on Site: vpcId and peerVpcId have the same value")
		return nil
	}

	vpcDAO := cdbm.NewVpcDAO(mvp.dbSession)
	vpcPeeringDAO := cdbm.NewVpcPeeringDAO(mvp.dbSession)
	statusDetailDAO := cdbm.NewStatusDetailDAO(mvp.dbSession)

	// Check for duplicate VPC Peering
	peerings, _, err := vpcPeeringDAO.GetAll(ctx, nil, cdbm.VpcPeeringFilterInput{
		VpcIDs: []uuid.UUID{reportedVpcPeering.Vpc1ID},
	}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get VPC Peerings for VPC from DB")
		return nil
	}

	for _, peering := range peerings {
		// The filter matches either end, so one side is already vpc1 and only the other needs checking
		if peering.Vpc1ID == reportedVpcPeering.Vpc2ID || peering.Vpc2ID == reportedVpcPeering.Vpc2ID {
			logger.Warn().Msgf("unable to create VPC Peering found on Site: an existing VPC Peering already connects the VPCs of VPC Peering: %s", controllerVpcPeeringID)
			return nil
		}
	}

	vpcPeering, err := cdb.WithTxResult(ctx, mvp.dbSession, func(tx *cdb.Tx) (*cdbm.VpcPeering, error) {
		vpcs, _, vpcErr := vpcDAO.GetAll(ctx, tx, cdbm.VpcFilterInput{
			VpcIDs:  []uuid.UUID{reportedVpcPeering.Vpc1ID, reportedVpcPeering.Vpc2ID},
			SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if vpcErr != nil {
			return nil, fmt.Errorf("unable to create VPC Peering found on Site: failed to retrieve VPCs by ID, DB error: %w", vpcErr)
		}

		vpcByID := make(map[uuid.UUID]*cdbm.Vpc, len(vpcs))
		for i := range vpcs {
			vpcByID[vpcs[i].ID] = &vpcs[i]
		}
		vpc1 := vpcByID[reportedVpcPeering.Vpc1ID]
		if vpc1 == nil {
			logger.Warn().Msgf("unable to create VPC Peering found on Site: no VPC was found for ID: %s", reportedVpcPeering.Vpc1ID)
			return nil, nil
		}
		vpc2 := vpcByID[reportedVpcPeering.Vpc2ID]
		if vpc2 == nil {
			logger.Warn().Msgf("unable to create VPC Peering found on Site: no peer VPC was found for ID: %s", reportedVpcPeering.Vpc2ID)
			return nil, nil
		}

		isMultiTenant := vpc1.TenantID != vpc2.TenantID
		var infrastructureProviderID *uuid.UUID
		var tenantID *uuid.UUID
		if isMultiTenant {
			infrastructureProviderID = cwutil.GetPtr(site.InfrastructureProviderID)
		} else {
			tenantID = cwutil.GetPtr(vpc1.TenantID)
		}

		matches, _, reloadErr := vpcPeeringDAO.GetAll(ctx, tx, cdbm.VpcPeeringFilterInput{
			IDs:            []uuid.UUID{controllerVpcPeeringID},
			IncludeDeleted: true,
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if reloadErr != nil {
			return nil, fmt.Errorf("unable to create VPC Peering found on Site: failed to retrieve VPC Peering by controller ID, DB error: %w", reloadErr)
		}

		var existingVpcPeering *cdbm.VpcPeering
		if len(matches) > 0 {
			existingVpcPeering = &matches[0]
		}
		if existingVpcPeering != nil {
			if existingVpcPeering.SiteID != site.ID {
				logger.Warn().Msgf("unable to create VPC Peering found on Site: VPC Peering ID already exists under a different Site for VPC Peering %s", controllerVpcPeeringID)
				return nil, nil
			}
			if existingVpcPeering.Deleted == nil {
				return existingVpcPeering, nil
			}

			sameVpcPair := (existingVpcPeering.Vpc1ID == vpc1.ID && existingVpcPeering.Vpc2ID == vpc2.ID) ||
				(existingVpcPeering.Vpc1ID == vpc2.ID && existingVpcPeering.Vpc2ID == vpc1.ID)
			if !sameVpcPair || existingVpcPeering.IsMultiTenant != isMultiTenant {
				logger.Warn().Msgf("unable to create VPC Peering found on Site: VPC pair differs in REST cache and Site record for VPC Peering %s", controllerVpcPeeringID)
				return nil, nil
			}

			// Deleted records when the delete happened, so a delete newer than the interval can
			// postdate this inventory. Undeleting then would revive a VPC Peering the snapshot
			// never saw removed. A later inventory undeletes it if the Site still reports it.
			if site.IsTimeWithinStaleInventoryThreshold(*existingVpcPeering.Deleted) {
				logger.Info().Msgf("not undeleting VPC Peering %s yet because it was deleted more recently than the inventory interval", controllerVpcPeeringID)
				return nil, nil
			}

			restored, clearErr := vpcPeeringDAO.Clear(ctx, tx, cdbm.VpcPeeringClearInput{
				VpcPeeringID: existingVpcPeering.ID,
				Deleted:      true,
			})
			if clearErr != nil {
				return nil, fmt.Errorf("unable to create VPC Peering found on Site: failed to clear soft-delete timestamp for VPC Peering, DB error: %w", clearErr)
			}

			status := cdbm.VpcPeeringStatusReady
			statusMessage := "VPC Peering has been re-detected on Site"
			statusErr := mvp.updateVpcPeeringStatusInDB(ctx, tx, restored.ID, &status, &statusMessage)
			if statusErr != nil {
				return nil, fmt.Errorf("unable to create VPC Peering found on Site: failed to update VPC Peering status after undelete, DB error: %w", statusErr)
			}

			restored, reloadErr = vpcPeeringDAO.GetByID(ctx, tx, restored.ID, nil)
			if reloadErr != nil {
				return nil, fmt.Errorf("unable to create VPC Peering found on Site: failed to retrieve VPC Peering after undelete, DB error: %w", reloadErr)
			}
			return restored, nil
		}

		readyMessage := "VPC Peering was found on Site, Ready for use"
		created, createErr := vpcPeeringDAO.Create(ctx, tx, cdbm.VpcPeeringCreateInput{
			VpcPeeringID:             &controllerVpcPeeringID,
			Vpc1ID:                   vpc1.ID,
			Vpc2ID:                   vpc2.ID,
			SiteID:                   site.ID,
			IsMultiTenant:            isMultiTenant,
			InfrastructureProviderID: infrastructureProviderID,
			TenantID:                 tenantID,
			Status:                   cdbm.VpcPeeringStatusReady,
			CreatedByID:              site.CreatedBy,
		})
		if createErr != nil {
			return nil, fmt.Errorf("unable to create VPC Peering found on Site: failed to create VPC Peering, DB error: %w", createErr)
		}

		_, statusErr := statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(),
			Status:   cdbm.VpcPeeringStatusReady,
			Message:  &readyMessage,
		})
		if statusErr != nil {
			return nil, fmt.Errorf("unable to create VPC Peering found on Site: failed to create Status Detail, DB error: %w", statusErr)
		}
		return created, nil
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to recover VPC Peering from Site inventory")
		return nil
	}
	return vpcPeering
}

// updateVpcPeeringStatusInDB is helper function to write VpcPeering updates to DB
func (mvp ManageVpcPeering) updateVpcPeeringStatusInDB(ctx context.Context, tx *cdb.Tx, vpcPeeringID uuid.UUID, status *string, statusMessage *string) error {
	if status != nil {
		VpcPeeringDAO := cdbm.NewVpcPeeringDAO(mvp.dbSession)

		err := VpcPeeringDAO.UpdateStatusByID(ctx, tx, vpcPeeringID, *status)
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mvp.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpcPeeringID.String(), Status: *status, Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// NewManageVpcPeering returns a new ManageVpcPeering activity
func NewManageVpcPeering(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageVpcPeering {
	return ManageVpcPeering{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
