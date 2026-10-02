// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"maps"
	"net/http"
	"slices"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/samber/lo"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// ~~~~~ Machine GPU Stats Handler ~~~~~ //

// GetMachineGPUStatsHandler is the API Handler for retrieving GPU stats for machines at a site
type GetMachineGPUStatsHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetMachineGPUStatsHandler initializes and returns a new handler for machine GPU stats
func NewGetMachineGPUStatsHandler(dbSession *cdb.Session, cfg *config.Config) GetMachineGPUStatsHandler {
	return GetMachineGPUStatsHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve GPU stats for machines at a site
// @Description Returns GPU summary stats grouped by GPU name for machines at the specified site
// @Tags machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param orgName path string true "Name of NGC organization"
// @Param siteId query string true "Site ID"
// @Success 200 {array} model.APIMachineGPUStats
// @Router /v2/org/{orgName}/nico/machine/gpu/stats [get]
func (gmgsh GetMachineGPUStatsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Machine", "GetGPUStats", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	infrastructureProvider, apiError := common.IsProvider(ctx, logger, gmgsh.dbSession, org, dbUser, false)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	siteIDStr := c.QueryParam("siteId")
	if siteIDStr == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}
	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gmgsh.dbSession)
	if err != nil {
		logger.Error().Err(err).Str("siteId", siteIDStr).Msg("error parsing or retrieving site")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID specified in query param", nil)
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to the specified site", nil)
	}

	// Aggregate GPU stats for the site in the database.
	gpuStats, err := common.GetSiteGPUStats(ctx, nil, gmgsh.dbSession, logger, &infrastructureProvider.ID, &site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving GPU stats for site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve GPU stats", nil)
	}

	result := gpuStats[site.ID]
	if result == nil {
		result = []model.APIMachineGPUStats{}
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, result)
}

// ~~~~~ Tenant Instance Type Stats Handler ~~~~~ //

// GetTenantInstanceTypeStatsHandler is the API Handler for retrieving per-tenant instance type allocation stats
type GetTenantInstanceTypeStatsHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetTenantInstanceTypeStatsHandler initializes and returns a new handler for tenant instance type stats
func NewGetTenantInstanceTypeStatsHandler(dbSession *cdb.Session, cfg *config.Config) GetTenantInstanceTypeStatsHandler {
	return GetTenantInstanceTypeStatsHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve per-tenant instance type allocation stats for a site
// @Description Returns instance type allocation stats grouped by tenant for the specified site
// @Tags tenant
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param orgName path string true "Name of NGC organization"
// @Param siteId query string true "Site ID"
// @Success 200 {array} model.APITenantInstanceTypeStats
// @Router /v2/org/{orgName}/nico/tenant/instance-type/stats [get]
func (gtitsh GetTenantInstanceTypeStatsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tenant", "GetInstanceTypeStats", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	infrastructureProvider, apiError := common.IsProvider(ctx, logger, gtitsh.dbSession, org, dbUser, false)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	siteIDStr := c.QueryParam("siteId")
	if siteIDStr == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}
	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gtitsh.dbSession)
	if err != nil {
		logger.Error().Err(err).Str("siteId", siteIDStr).Msg("error parsing or retrieving site")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID specified in query param", nil)
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to the specified site", nil)
	}

	siteID := &site.ID
	siteIDs := []uuid.UUID{*siteID}

	// 1. Fetch all instance types for the site
	itDAO := cdbm.NewInstanceTypeDAO(gtitsh.dbSession)
	instanceTypes, _, err := itDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{SiteIDs: siteIDs},
		nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instance types")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve instance types", nil)
	}

	instanceTypeIDs := lo.Map(instanceTypes, func(it cdbm.InstanceType, _ int) uuid.UUID { return it.ID })
	instanceTypeMap := lo.KeyBy(instanceTypes, func(it cdbm.InstanceType) uuid.UUID { return it.ID })

	// 2. Fetch all allocations for the site (IDs needed to filter constraints)
	aDAO := cdbm.NewAllocationDAO(gtitsh.dbSession)
	allocations, _, err := aDAO.GetAll(ctx, nil, cdbm.AllocationFilterInput{SiteIDs: siteIDs},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving allocations")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve allocations", nil)
	}

	allocationIDs := lo.Map(allocations, func(a cdbm.Allocation, _ int) uuid.UUID { return a.ID })

	// 3. Fetch allocation constraints with Allocation.Tenant
	var constraints []cdbm.AllocationConstraint
	if len(allocationIDs) > 0 {
		acDAO := cdbm.NewAllocationConstraintDAO(gtitsh.dbSession)
		constraints, _, err = acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{
			AllocationIDs:   allocationIDs,
			ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
			ResourceTypeIDs: instanceTypeIDs,
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{"Allocation.Tenant"})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving allocation constraints")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve allocation constraints", nil)
		}
	}

	// 4. Fetch all instances for the site
	iDAO := cdbm.NewInstanceDAO(gtitsh.dbSession)
	instances, _, err := iDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{SiteIDs: siteIDs},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instances")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve instances", nil)
	}

	// 5. Fetch all machines with instance types for the site (exclude metadata)
	machineDAO := cdbm.NewMachineDAO(gtitsh.dbSession)
	machines, _, err := machineDAO.GetAll(ctx, nil, cdbm.MachineFilterInput{
		SiteIDs:         []uuid.UUID{site.ID},
		InstanceTypeIDs: instanceTypeIDs,
		ExcludeMetadata: true,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving machines")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve machines", nil)
	}

	machineByID := lo.KeyBy(machines, func(m cdbm.Machine) string { return m.ID })

	// Build usage maps
	tenantITUsed := make(map[uuid.UUID]map[uuid.UUID]*model.APIMachineStatusBreakdown)
	for _, inst := range instances {
		if inst.InstanceTypeID == nil || inst.MachineID == nil {
			continue
		}
		tID := inst.TenantID
		itID := *inst.InstanceTypeID
		if tenantITUsed[tID] == nil {
			tenantITUsed[tID] = make(map[uuid.UUID]*model.APIMachineStatusBreakdown)
		}
		if tenantITUsed[tID][itID] == nil {
			tenantITUsed[tID][itID] = &model.APIMachineStatusBreakdown{}
		}
		if m, ok := machineByID[*inst.MachineID]; ok {
			tenantITUsed[tID][itID].AddMachineStatusCounts(m)
		}
	}

	// Group constraints by tenantID -> instanceTypeID
	tenantITAllocs := make(map[uuid.UUID]map[uuid.UUID][]cdbm.AllocationConstraint)
	for _, ac := range constraints {
		tID := ac.Allocation.TenantID
		itID := ac.ResourceTypeID
		if tenantITAllocs[tID] == nil {
			tenantITAllocs[tID] = make(map[uuid.UUID][]cdbm.AllocationConstraint)
		}
		tenantITAllocs[tID][itID] = append(tenantITAllocs[tID][itID], ac)
	}

	// Ready assigned machines per instance type
	readyAssignedMachines := lo.Filter(machines, func(m cdbm.Machine, _ int) bool {
		return m.InstanceTypeID != nil && m.Status == cdbm.MachineStatusReady
	})
	readyMachineCountByIT := lo.CountValuesBy(readyAssignedMachines, func(m cdbm.Machine) uuid.UUID { return *m.InstanceTypeID })

	// Calculate total allocated per instance type across all tenants
	totalAllocatedByIT := lo.Reduce(constraints, func(acc map[uuid.UUID]int, ac cdbm.AllocationConstraint, _ int) map[uuid.UUID]int {
		acc[ac.ResourceTypeID] += ac.ConstraintValue
		return acc
	}, make(map[uuid.UUID]int))

	// Calculate total machines in use per instance type across all tenants
	// This counts machines with running instances regardless of their status
	totalInUseByIT := make(map[uuid.UUID]int)
	for _, itUsageMap := range tenantITUsed {
		for itID, breakdown := range itUsageMap {
			totalInUseByIT[itID] += breakdown.Total
		}
	}

	// Build response grouped by tenant
	tenantStatsMap := make(map[uuid.UUID]model.APITenantInstanceTypeStats)
	for tID, itAllocs := range tenantITAllocs {
		var t *cdbm.Tenant
		for _, acs := range itAllocs {
			if len(acs) > 0 {
				t = acs[0].Allocation.Tenant
				break
			}
		}

		tenantOrgDisplay := t.Org
		if t.OrgDisplayName != nil {
			tenantOrgDisplay = *t.OrgDisplayName
		}

		ts := model.APITenantInstanceTypeStats{
			ID:             t.ID.String(),
			Org:            t.Org,
			OrgDisplayName: tenantOrgDisplay,
		}

		for itID, details := range itAllocs {
			it, ok := instanceTypeMap[itID]
			if !ok {
				continue
			}
			allocated := lo.Reduce(details, func(acc int, ac cdbm.AllocationConstraint, _ int) int {
				return acc + ac.ConstraintValue
			}, 0)

			apiAllocs := lo.Map(details, func(ac cdbm.AllocationConstraint, _ int) model.APITenantInstanceTypeAllocationStats {
				return model.APITenantInstanceTypeAllocationStats{
					ID:    ac.Allocation.ID.String(),
					Name:  ac.Allocation.Name,
					Total: ac.ConstraintValue,
				}
			})

			used := model.APIMachineStatusBreakdown{}
			if tenantITUsed[tID] != nil && tenantITUsed[tID][itID] != nil {
				used = *tenantITUsed[tID][itID]
			}

			// ready machines minus those already reserved (allocated but not yet in use)
			maxAlloc := max(0, readyMachineCountByIT[itID]-(totalAllocatedByIT[itID]-totalInUseByIT[itID]))

			ts.InstanceTypes = append(ts.InstanceTypes, model.APITenantInstanceTypeStatsEntry{
				ID:               it.ID.String(),
				Name:             it.Name,
				Allocated:        allocated,
				UsedMachineStats: used,
				MaxAllocatable:   maxAlloc,
				Allocations:      apiAllocs,
			})
		}

		tenantStatsMap[tID] = ts
	}

	result := slices.Collect(maps.Values(tenantStatsMap))

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, result)
}

// ~~~~~ Machine Instance Type Summary Handler ~~~~~ //

// GetMachineInstanceTypeSummaryHandler is the API Handler for retrieving assigned vs unassigned machine summary
type GetMachineInstanceTypeSummaryHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetMachineInstanceTypeSummaryHandler initializes and returns a new handler for machine instance type summary
func NewGetMachineInstanceTypeSummaryHandler(dbSession *cdb.Session, cfg *config.Config) GetMachineInstanceTypeSummaryHandler {
	return GetMachineInstanceTypeSummaryHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve machine instance type assignment summary for a site
// @Description Returns machine counts grouped by assigned (has instance type) vs unassigned, broken down by status
// @Tags machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param orgName path string true "Name of NGC organization"
// @Param siteId query string true "Site ID"
// @Success 200 {object} model.APIMachineInstanceTypeSummary
// @Router /v2/org/{orgName}/nico/machine/instance-type/stats/summary [get]
func (gmitsh GetMachineInstanceTypeSummaryHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Machine", "GetInstanceTypeSummary", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	infrastructureProvider, apiError := common.IsProvider(ctx, logger, gmitsh.dbSession, org, dbUser, false)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	siteIDStr := c.QueryParam("siteId")
	if siteIDStr == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}
	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gmitsh.dbSession)
	if err != nil {
		logger.Error().Err(err).Str("siteId", siteIDStr).Msg("error parsing or retrieving site")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID specified in query param", nil)
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to the specified site", nil)
	}

	// Fetch all machines for the site (exclude metadata for performance)
	machineDAO := cdbm.NewMachineDAO(gmitsh.dbSession)
	machines, _, err := machineDAO.GetAll(ctx, nil, cdbm.MachineFilterInput{
		SiteIDs:         []uuid.UUID{site.ID},
		ExcludeMetadata: true,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving machines for site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve machines", nil)
	}

	// Partition into assigned vs unassigned, count by status
	var assigned, unassigned model.APIMachineStatusBreakdown
	for _, m := range machines {
		bd := &assigned
		if m.InstanceTypeID == nil {
			bd = &unassigned
		}
		bd.AddMachineStatusCounts(m)
	}

	result := model.APIMachineInstanceTypeSummary{
		Assigned:   assigned,
		Unassigned: unassigned,
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, result)
}

// ~~~~~ Machine Instance Type Detailed Stats Handler ~~~~~ //

// GetMachineInstanceTypeStatsHandler is the API Handler for retrieving detailed per-instance-type machine stats
type GetMachineInstanceTypeStatsHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetMachineInstanceTypeStatsHandler initializes and returns a new handler for machine instance type stats
func NewGetMachineInstanceTypeStatsHandler(dbSession *cdb.Session, cfg *config.Config) GetMachineInstanceTypeStatsHandler {
	return GetMachineInstanceTypeStatsHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve detailed per-instance-type machine stats for a site
// @Description Returns machine stats for each instance type including allocation details and tenant breakdown
// @Tags machine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param orgName path string true "Name of NGC organization"
// @Param siteId query string true "Site ID"
// @Success 200 {array} model.APIMachineInstanceTypeStats
// @Router /v2/org/{orgName}/nico/machine/instance-type/stats [get]
func (gmitsh GetMachineInstanceTypeStatsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Machine", "GetInstanceTypeStats", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	infrastructureProvider, apiError := common.IsProvider(ctx, logger, gmitsh.dbSession, org, dbUser, false)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	siteIDStr := c.QueryParam("siteId")
	if siteIDStr == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}
	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gmitsh.dbSession)
	if err != nil {
		logger.Error().Err(err).Str("siteId", siteIDStr).Msg("error parsing or retrieving site")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID specified in query param", nil)
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to the specified site", nil)
	}

	siteIDs := []uuid.UUID{site.ID}

	// 1. Fetch all instance types for the site
	itDAO := cdbm.NewInstanceTypeDAO(gmitsh.dbSession)
	instanceTypes, _, err := itDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{SiteIDs: siteIDs},
		nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instance types")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve instance types", nil)
	}

	if len(instanceTypes) == 0 {
		return c.JSON(http.StatusOK, []model.APIMachineInstanceTypeStats{})
	}

	instanceTypeIDs := lo.Map(instanceTypes, func(it cdbm.InstanceType, _ int) uuid.UUID { return it.ID })

	// 2. Fetch all machines for the site (exclude metadata)
	machineDAO := cdbm.NewMachineDAO(gmitsh.dbSession)
	machines, _, err := machineDAO.GetAll(ctx, nil, cdbm.MachineFilterInput{
		SiteIDs:         siteIDs,
		ExcludeMetadata: true,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving machines")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve machines", nil)
	}

	machineByID := lo.KeyBy(machines, func(m cdbm.Machine) string { return m.ID })

	assignedMachines := lo.Filter(machines, func(m cdbm.Machine, _ int) bool { return m.InstanceTypeID != nil })
	machinesByIT := lo.GroupBy(assignedMachines, func(m cdbm.Machine) uuid.UUID { return *m.InstanceTypeID })

	// 3. Fetch all allocations for the site (IDs needed to filter constraints)
	aDAO := cdbm.NewAllocationDAO(gmitsh.dbSession)
	allocations, _, err := aDAO.GetAll(ctx, nil, cdbm.AllocationFilterInput{SiteIDs: siteIDs},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving allocations")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve allocations", nil)
	}

	allocationIDs := lo.Map(allocations, func(a cdbm.Allocation, _ int) uuid.UUID { return a.ID })

	// 4. Fetch allocation constraints with Allocation.Tenant
	var constraints []cdbm.AllocationConstraint
	if len(allocationIDs) > 0 {
		acDAO := cdbm.NewAllocationConstraintDAO(gmitsh.dbSession)
		constraints, _, err = acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{
			AllocationIDs:   allocationIDs,
			ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
			ResourceTypeIDs: instanceTypeIDs,
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{"Allocation.Tenant"})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving allocation constraints")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve allocation constraints", nil)
		}
	}

	// 5. Fetch all instances for the site
	iDAO := cdbm.NewInstanceDAO(gmitsh.dbSession)
	instances, _, err := iDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{SiteIDs: siteIDs},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instances")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve instances", nil)
	}

	// Build aggregation maps using shared helpers
	itUsed, tenantITUsed := model.GetInstanceTypeMachineUsageMap(instances, machineByID)
	constraintsByIT := make(map[uuid.UUID][]cdbm.AllocationConstraint)
	for _, ac := range constraints {
		constraintsByIT[ac.ResourceTypeID] = append(constraintsByIT[ac.ResourceTypeID], ac)
	}

	// Build response
	result := lo.Map(instanceTypes, func(it cdbm.InstanceType, _ int) model.APIMachineInstanceTypeStats {
		return model.NewAPIMachineInstanceTypeStats(it, machinesByIT[it.ID], constraintsByIT[it.ID], itUsed, tenantITUsed)
	})

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, result)
}
