// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tenant

import (
	"context"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/client"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ManageTenant is an activity wrapper for managing Tenant lifecycle that allows
// injecting DB access
type ManageTenant struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// UpdateTenantsInDB is a Temporal activity that takes a collection of Tenant data pushed by Site Agent and updates the DB
func (mt ManageTenant) UpdateTenantsInDB(ctx context.Context, siteID uuid.UUID, tenantInventory *corev1.TenantInventory) error {
	logger := log.With().Str("Activity", "UpdateTenantsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mt.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received Tenant inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	// Get temporal client for specified Site
	tc, err := mt.siteClientPool.GetClientByID(siteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return err
	}

	if tenantInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	tenantSiteDAO := cdbm.NewTenantSiteDAO(mt.dbSession)

	tenantSites, _, err := tenantSiteDAO.GetAll(
		ctx,
		nil,
		cdbm.TenantSiteFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		},
		cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.TenantRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get Tenant Site associations from DB")
		return err
	}

	existingTenants := []cdbm.Tenant{}
	for _, tenantSite := range tenantSites {
		existingTenants = append(existingTenants, *tenantSite.Tenant)
	}

	// Construct a map of Controller Tenant ID to Tenant
	existingTenantOrgMap := make(map[string]*cdbm.Tenant)

	for _, tenant := range existingTenants {
		curTenant := tenant
		existingTenantOrgMap[tenant.Org] = &curTenant
	}

	reportedTenantOrgMap := map[string]bool{}

	if tenantInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Tenant inventory page: %d of %d, page size: %d, total count: %d",
			tenantInventory.InventoryPage.CurrentPage, tenantInventory.InventoryPage.TotalPages,
			tenantInventory.InventoryPage.PageSize, tenantInventory.InventoryPage.TotalItems)

		for _, orgID := range tenantInventory.InventoryPage.ItemIds {
			reportedTenantOrgMap[orgID] = true
		}
	}

	// Iterate through Tenant Inventory and update DB
	for _, controllerTenant := range tenantInventory.Tenants {
		slogger := logger.With().Str("Tenant Org", controllerTenant.OrganizationId).Logger()

		tenant, ok := existingTenantOrgMap[controllerTenant.OrganizationId]
		if !ok {
			slogger.Error().Msg("Tenant does not have a record in DB, possibly created directly on Site")
			continue
		}

		reportedTenantOrgMap[tenant.Org] = true

		// Verify if Tenant's metadata update required, if yes trigger `UpdateTenant` workflow
		if controllerTenant.Metadata != nil {
			triggerTenantMetadataUpdate := false

			if *tenant.OrgDisplayName != controllerTenant.Metadata.Name {
				triggerTenantMetadataUpdate = true
			}

			// Trigger update Tenant workflow
			if triggerTenantMetadataUpdate {
				slogger.Info().Msg("detected data out of sync, triggering Tenant update workflow")
				_ = mt.CreateOrUpdateTenantOnSite(ctx, siteID, tc, tenant, controllerTenant)
			}
		}
	}

	// Populate list of Tenants that were not found
	tenantsToCreate := []*cdbm.Tenant{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if tenantInventory.InventoryPage == nil || tenantInventory.InventoryPage.TotalPages == 0 || (tenantInventory.InventoryPage.CurrentPage == tenantInventory.InventoryPage.TotalPages) {
		for _, tenant := range existingTenantOrgMap {
			found := false

			_, found = reportedTenantOrgMap[tenant.Org]
			if !found {
				// The Tenant was not found in the Tenant Inventory, so add it to list of Tenants to potentially terminate
				tenantsToCreate = append(tenantsToCreate, tenant)
			}
		}
	}

	// Loop through Tenants for creation on Site
	for _, tenant := range tenantsToCreate {
		slogger := logger.With().Str("Tenant Org", tenant.Org).Logger()

		// Was this created within inventory receipt interval? If so, we may be processing an older inventory
		if site.IsTimeWithinStaleInventoryThreshold(tenant.Created) {
			continue
		}

		// Trigger Tenant creation on Site
		slogger.Info().Msg("Tenant not detected on Site, triggering Tenant create workflow")
		_ = mt.CreateOrUpdateTenantOnSite(ctx, siteID, tc, tenant, nil)
	}

	return nil
}

// CreateOrUpdateTenantOnSite is a Temporal activity to create or update Tenants on Site
func (mt ManageTenant) CreateOrUpdateTenantOnSite(ctx context.Context, siteID uuid.UUID, tc client.Client, tenant *cdbm.Tenant, controllerTenant *corev1.Tenant) error {
	logger := log.With().Str("Activity", "CreateOrUpdateTenantOnSite").Str("Site ID", siteID.String()).Str("Tenant Org", tenant.Org).Logger()

	logger.Info().Msg("starting activity")

	// Build an update request for tenant that needs a sync metadata and call UpdateTenant.
	if controllerTenant == nil {
		workflowOptions := client.StartWorkflowOptions{
			ID:        "site-tenant-create-" + tenant.Org,
			TaskQueue: queue.SiteTaskQueue,
		}

		// Trigger apporpriate workflow on Site
		createTenantRequest := tenant.ToCreateRequestProto()

		we, err := tc.ExecuteWorkflow(ctx, workflowOptions, "CreateTenant", createTenantRequest)
		if err != nil {
			logger.Error().Err(err).Str("Tenant ID", tenant.ID.String()).Msg("failed to trigger workflow to create Tenant")
		} else {
			logger.Info().Str("Workflow ID", we.GetID()).Msg("triggered workflow to create Tenant")
		}
	} else {
		workflowOptions := client.StartWorkflowOptions{
			ID:        "site-tenant-update-" + tenant.Org,
			TaskQueue: queue.SiteTaskQueue,
		}

		// Trigger apporpriate workflow on Site
		updateTenantRequest := tenant.ToUpdateRequestProto()

		// Populate the RoutingProfileType directly from what was sent from the controller
		// until/unless we start storing this detail in the REST DB.
		updateTenantRequest.RoutingProfileType = controllerTenant.RoutingProfileType

		we, err := tc.ExecuteWorkflow(ctx, workflowOptions, "UpdateTenant", updateTenantRequest)
		if err != nil {
			logger.Error().Err(err).Str("Tenant ID", tenant.ID.String()).Msg("failed to trigger workflow to update Tenant")
		} else {
			logger.Info().Str("Workflow ID", we.GetID()).Msg("triggered workflow to update Tenant")
		}
	}

	logger.Info().Msg("completed activity")

	return nil
}

// NewManageTenant returns a new ManageTenant activity
func NewManageTenant(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageTenant {
	return ManageTenant{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
