// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package networksecuritygroup

import (
	"context"
	"errors"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ManageNetworkSecurityGroup is an activity wrapper for managing NetworkSecurityGroup lifecycle that allows
// injecting DB access
type ManageNetworkSecurityGroup struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateNetworkSecurityGroupsInDB is a Temporal activity that takes a collection of NetworkSecurityGroup data pushed by Site Agent and updates the DB
func (mv ManageNetworkSecurityGroup) UpdateNetworkSecurityGroupsInDB(ctx context.Context, siteID uuid.UUID, networkSecurityGroupInventory *corev1.NetworkSecurityGroupInventory) error {
	logger := log.With().Str("Activity", "UpdateNetworkSecurityGroupsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if networkSecurityGroupInventory == nil {
		logger.Error().Msg("UpdateNetworkSecurityGroupsInDB called with nil inventory")
		return errors.New("UpdateNetworkSecurityGroupsInDB called with nil inventory")
	}

	if networkSecurityGroupInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	stDAO := cdbm.NewSiteDAO(mv.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received NetworkSecurityGroup inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	tenantDAO := cdbm.NewTenantDAO(mv.dbSession)
	tenantOrgToIDs := map[string]uuid.UUID{}

	networkSecurityGroupDAO := cdbm.NewNetworkSecurityGroupDAO(mv.dbSession)

	existingNetworkSecurityGroups, _, err := networkSecurityGroupDAO.GetAll(ctx, nil, cdbm.NetworkSecurityGroupFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Offset: nil, OrderBy: nil, Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get NetworkSecurityGroups for Site from DB")
		return err
	}

	// Map of NetworkSecurityGroups known to cloud
	existingNetworkSecurityGroupIDMap := make(map[string]*cdbm.NetworkSecurityGroup)
	for _, networkSecurityGroup := range existingNetworkSecurityGroups {
		existingNetworkSecurityGroupIDMap[networkSecurityGroup.ID] = &networkSecurityGroup
	}

	// Map of NetworkSecurityGroups known to site
	reportedNetworkSecurityGroupIDMap := map[string]bool{}
	if networkSecurityGroupInventory.InventoryPage != nil {
		for _, itID := range networkSecurityGroupInventory.InventoryPage.ItemIds {
			reportedNetworkSecurityGroupIDMap[itID] = true
		}
	}

	// Iterate through NetworkSecurityGroup Inventory and update DB
	for _, controllerNetworkSecurityGroup := range networkSecurityGroupInventory.NetworkSecurityGroups {

		networkSecurityGroup, found := existingNetworkSecurityGroupIDMap[controllerNetworkSecurityGroup.Id]

		// If we find anything on site that isn't in cloud,
		// add it to cloud.
		slogger := logger.With().Str("Controller NetworkSecurityGroup ID", controllerNetworkSecurityGroup.Id).Logger()

		if !found {

			slogger.Warn().Msg("NetworkSecurityGroup does not have a record in DB, possibly created directly on Site")
			slogger.Info().Msg("Creating record in DB")

			tenantID, foundTenant := tenantOrgToIDs[controllerNetworkSecurityGroup.TenantOrganizationId]

			// cloud-db has no generic GetAll for Tenant.
			// The closest we have is GetAllByOrg.  We can probably
			// stick with that for now and just memoize here as we see
			// tenants, so if we see a tenant we don't know about, we'll
			// query and cache it.
			if !foundTenant {
				tenants, _, err := tenantDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{controllerNetworkSecurityGroup.TenantOrganizationId}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
				if err != nil {
					slogger.Error().Err(err).Msg("failed to query for tenant ID for " + controllerNetworkSecurityGroup.TenantOrganizationId)
					return err
				}

				if len(tenants) == 0 {
					slogger.Error().Msg("failed to find tenant ID for " + controllerNetworkSecurityGroup.TenantOrganizationId)
					continue
				} else if len(tenants) > 1 {
					slogger.Warn().Msg("selecting first of many tenant IDs found for " + controllerNetworkSecurityGroup.TenantOrganizationId)
				}

				tenantID = tenants[0].ID
				tenantOrgToIDs[controllerNetworkSecurityGroup.TenantOrganizationId] = tenantID
			}

			rules := make([]*cdbm.NetworkSecurityGroupRule, len(controllerNetworkSecurityGroup.GetAttributes().GetRules()))
			for i, rule := range controllerNetworkSecurityGroup.GetAttributes().GetRules() {
				rules[i] = &cdbm.NetworkSecurityGroupRule{NetworkSecurityGroupRuleAttributes: rule}
			}

			// Add the new NetworkSecurityGroup
			_, err = networkSecurityGroupDAO.Create(ctx, nil, cdbm.NetworkSecurityGroupCreateInput{
				NetworkSecurityGroupID: &controllerNetworkSecurityGroup.Id,
				Name:                   controllerNetworkSecurityGroup.GetMetadata().GetName(),
				Description:            cwutil.GetPtr(controllerNetworkSecurityGroup.GetMetadata().GetDescription()),
				TenantOrg:              controllerNetworkSecurityGroup.TenantOrganizationId,
				TenantID:               tenantID,
				StatefulEgress:         controllerNetworkSecurityGroup.GetAttributes().GetStatefulEgress(),
				Rules:                  rules,
				SiteID:                 siteID,
				Status:                 cdbm.NetworkSecurityGroupStatusReady,
				Version:                &controllerNetworkSecurityGroup.Version,
				CreatedByID:            siteID, /* This would normally be a user ID, but that isn't something NICo provides */
			})

			if err != nil {
				slogger.Error().Err(err).Msg("NetworkSecurityGroup could not be created")
				continue
			}

			slogger.Info().Msg("NetworkSecurityGroup has been created from Site data")

		} else {
			// NOTE:	This is redundant if paging is used because we built he map earlier,
			//			but this isn't expensive.
			reportedNetworkSecurityGroupIDMap[networkSecurityGroup.ID] = true

			// A row written since the Site collected this inventory holds changes the snapshot
			// cannot know about, including rules set through the API, so writing the reported
			// values over them would lose those edits.
			if site.IsTimeWithinStaleInventoryThreshold(networkSecurityGroup.Updated) {
				slogger.Info().Msg("not updating NetworkSecurityGroup yet because it changed more recently than the inventory interval")

				continue
			}

			if networkSecurityGroup.Version != controllerNetworkSecurityGroup.Version {
				// If the record coming in from site is known to cloud but site
				// reports a different version, time to update cloud.

				rules := make([]*cdbm.NetworkSecurityGroupRule, len(controllerNetworkSecurityGroup.GetAttributes().GetRules()))
				for i, rule := range controllerNetworkSecurityGroup.GetAttributes().GetRules() {
					rules[i] = &cdbm.NetworkSecurityGroupRule{NetworkSecurityGroupRuleAttributes: rule}
				}

				_, err = networkSecurityGroupDAO.Update(ctx, nil, cdbm.NetworkSecurityGroupUpdateInput{
					NetworkSecurityGroupID: controllerNetworkSecurityGroup.Id,
					Name:                   cwutil.GetPtr(controllerNetworkSecurityGroup.GetMetadata().GetName()),
					Description:            cwutil.GetPtr(controllerNetworkSecurityGroup.GetMetadata().GetDescription()),
					StatefulEgress:         cwutil.GetPtr(controllerNetworkSecurityGroup.GetAttributes().GetStatefulEgress()),
					Rules:                  rules,
					Status:                 cwutil.GetPtr(cdbm.NetworkSecurityGroupStatusReady),
					Version:                &controllerNetworkSecurityGroup.Version,
					UpdatedByID:            siteID, /* This would normally be a user ID, but that isn't something NICo provides */
				})

				if err != nil {
					slogger.Error().Err(err).Msg("NetworkSecurityGroup could not be updated")
					continue
				}

				slogger.Info().Msg("NetworkSecurityGroup data has been updated from Site data")
			}
		}
	}

	// We only need to do this once.
	// If inventory paging is enabled, we populated reportedNetworkSecurityGroupIDMap much earlier,
	// and we can do the deletion processing on the last page.
	// Otherwise, it's happening right away because there's no paging,
	// and the call to this function was a one-shot with all inventory,
	// and reportedNetworkSecurityGroupIDMap was populated while processing the unpaged
	// inventory.
	if networkSecurityGroupInventory.InventoryPage == nil || networkSecurityGroupInventory.InventoryPage.TotalPages == 0 || (networkSecurityGroupInventory.InventoryPage.CurrentPage == networkSecurityGroupInventory.InventoryPage.TotalPages) {

		// Clear out any that don't exist on site.
		for _, networkSecurityGroup := range existingNetworkSecurityGroupIDMap {
			slogger := logger.With().Str("NetworkSecurityGroup ID", networkSecurityGroup.ID).Logger()
			slogger.Info().Msg("checking for deletion")

			_, found := reportedNetworkSecurityGroupIDMap[networkSecurityGroup.ID]
			if !found {
				// The NetworkSecurityGroup was not found in the NetworkSecurityGroup Inventory,
				// so we should delete it, but we might be processing an older
				// inventory, so make sure the object has existed for at least as
				// long as our inventory interval with a little buffer to make
				// sure we aren't in lock-step.
				if site.IsTimeWithinStaleInventoryThreshold(networkSecurityGroup.Created) {
					slogger.Info().Msg("not going to delete yet because group is newer than the inventory interval")

					continue
				}

				slogger.Info().Msg("going to delete")

				serr := networkSecurityGroupDAO.Delete(ctx, nil, cdbm.NetworkSecurityGroupDeleteInput{NetworkSecurityGroupID: networkSecurityGroup.ID, UpdatedByID: site.ID})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to delete NetworkSecurityGroup from DB")
				}
			}
		}
	}

	return nil
}

// NewManageNetworkSecurityGroup returns a new ManageNetworkSecurityGroup activity
func NewManageNetworkSecurityGroup(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageNetworkSecurityGroup {
	return ManageNetworkSecurityGroup{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
