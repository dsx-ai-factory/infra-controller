// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"fmt"
	"net/http"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"

	goset "github.com/deckarep/golang-set/v2"
	"github.com/google/uuid"
	"github.com/samber/lo"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ~~~~~ GetAll Instance InfiniBandInterface Handler ~~~~~ //

// GetAllInstanceInfiniBandInterfaceHandler is the API Handler for retrieving all InfiniBandInterfaces for an Instance
type GetAllInstanceInfiniBandInterfaceHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllInstanceInfiniBandInterfaceHandler initializes and returns a new handler for retrieving all InfiniBandInterfaces for an Instance
func NewGetAllInstanceInfiniBandInterfaceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllInstanceInfiniBandInterfaceHandler {
	return GetAllInstanceInfiniBandInterfaceHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve all Interfaces for an Instance
// @Description Retrieve all Interfaces for an Instance
// @Tags interface
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param instanceId path string true "ID of Instance"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Instance', 'Subnet'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} model.APIInterface
// @Router /v2/org/{org}/nico/instance/{instance_id}/interface [get]
func (gaiibih GetAllInstanceInfiniBandInterfaceHandler) Handle(c echo.Context) error {
	instanceID := c.Param("instanceId")
	queryOverride := &common.QueryOverride{
		InstanceIDs:   []string{instanceID},
		ValueFromPath: true,
	}
	delegate := NewGetAllInfiniBandInterfaceHandler(gaiibih.dbSession, gaiibih.tc, gaiibih.cfg, queryOverride)
	return delegate.Handle(c)
}

// ~~~~~ GetAll InfiniBandInterface Handler ~~~~~ //

// GetAllInfiniBandInterfaceHandler is the API Handler for retrieving all InfiniBandInterfaces
type GetAllInfiniBandInterfaceHandler struct {
	dbSession     *cdb.Session
	tc            temporalClient.Client
	cfg           *config.Config
	queryOverride *common.QueryOverride
}

// NewGetAllInfiniBandInterfaceHandler initializes and returns a new handler for retrieving all InfiniBandInterfaces.
// When queryOverride is provided (e.g. when delegating from instance-scoped endpoint), it supplies values
// that would otherwise come from query params, and error messages use "path" instead of "query".
func NewGetAllInfiniBandInterfaceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config, queryOverride ...*common.QueryOverride) GetAllInfiniBandInterfaceHandler {
	var override *common.QueryOverride
	if len(queryOverride) > 0 {
		override = queryOverride[0]
	}
	return GetAllInfiniBandInterfaceHandler{
		dbSession:     dbSession,
		tc:            tc,
		cfg:           cfg,
		queryOverride: override,
	}
}

// Handle godoc
// @Summary Retrieve all InfiniBandInterfaces
// @Description Retrieve all InfiniBandInterfaces
// @Tags InfiniBandInterface
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param instanceId path string true "ID of Instance"
// @Param infinibandPartitionId path string true "ID of InfiniBandPartition"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfiniBandPartition, Instance'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} model.APIInfiniBandInterface
// @Router /v2/org/{org}/nico/infiniband-interface [get]
func (gaibih GetAllInfiniBandInterfaceHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfiniBandInterface", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Tenant Admins are allowed to retrieve Instances
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination request attributes
	err = pageRequest.Validate(cdbm.InfiniBandInterfaceOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InfiniBandInterfaceRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gaibih.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]

	// Get site IDs from query param - parse first, then bulk fetch
	var siteIDs []uuid.UUID
	siteIDStrs := qParams["siteId"]
	for _, siteIDStr := range siteIDStrs {
		parsedID, err := uuid.Parse(siteIDStr)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Site ID: %s specified in query", siteIDStr), nil)
		}
		siteIDs = append(siteIDs, parsedID)
	}

	if len(siteIDStrs) > 0 {
		// Set tracer span attribute
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("siteIds", siteIDStrs))

		// De-duplicate Site IDs
		siteIDs = goset.NewSet(siteIDs...).ToSlice()

		// Get all TenantSites for the Tenant and Sites specified in query
		tsDAO := cdbm.NewTenantSiteDAO(gaibih.dbSession)
		tenantSites, _, err := tsDAO.GetAll(
			ctx,
			nil,
			cdbm.TenantSiteFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
				SiteIDs:   siteIDs,
			},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			[]string{cdbm.SiteRelationName},
		)

		if err != nil {
			logger.Error().Err(err).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Sites specified in query, DB error", nil)
		}

		// Check if Tenant has access to each Site
		tenantSiteIDMap := map[uuid.UUID]*cdbm.TenantSite{}
		for i := range tenantSites {
			tenantSiteIDMap[tenantSites[i].SiteID] = &tenantSites[i]
		}

		for _, siteID := range siteIDs {
			// Check if Tenant has access to Site
			if _, ok := tenantSiteIDMap[siteID]; !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Site: %s specified in query doesn't exist or Tenant doesn't have access to it", siteID.String()), nil)
			}
		}
	}

	// Get Instance IDs - from queryOverride when delegating from path-scoped endpoint, else from query param
	var instanceIDs []uuid.UUID
	instanceIDFromPath := gaibih.queryOverride != nil && gaibih.queryOverride.ValueFromPath

	instanceIDStrs := qParams["instanceId"]
	if instanceIDFromPath && len(gaibih.queryOverride.InstanceIDs) > 0 {
		instanceIDStrs = gaibih.queryOverride.InstanceIDs
	}

	for _, instanceIDStr := range instanceIDStrs {
		parsedID, err := uuid.Parse(instanceIDStr)
		if err != nil {
			errMsg := fmt.Sprintf("Invalid Instance ID: %s in query", instanceIDStr)
			if instanceIDFromPath {
				errMsg = fmt.Sprintf("Invalid Instance ID: %s specified in request", instanceIDStr)
			}
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
		}
		instanceIDs = append(instanceIDs, parsedID)
	}

	if len(instanceIDStrs) > 0 {
		// Set tracer span attribute
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("instanceIds", instanceIDStrs))

		// De-duplicate Instance IDs
		instanceIDs = goset.NewSet(instanceIDs...).ToSlice()

		instanceDAO := cdbm.NewInstanceDAO(gaibih.dbSession)
		instances, _, err := instanceDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{InstanceIDs: instanceIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Instances from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve Instances specified in %s, DB error", lo.Ternary(instanceIDFromPath, "request", "query")), nil)
		}
		instanceIDMap := map[uuid.UUID]*cdbm.Instance{}

		for i := range instances {
			instanceIDMap[instances[i].ID] = &instances[i]
		}

		for _, instanceID := range instanceIDs {
			instance, ok := instanceIDMap[instanceID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Instance with ID: %s specified in %s",
					instanceID.String(), lo.Ternary(instanceIDFromPath, "request", "query")), nil)
			}

			if instance.TenantID != tenant.ID {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Instance: %s specified in %s doesn't belong to current Tenant",
					instanceID.String(), lo.Ternary(instanceIDFromPath, "request", "query")), nil)
			}
		}
	}

	// Get InfiniBand Partition IDs from query param - parse first, then bulk fetch
	var infiniBandPartitionIDs []uuid.UUID
	ibpIDStrs := qParams["infinibandPartitionId"]
	for _, ibpIDStr := range ibpIDStrs {
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("infinibandPartitionId", ibpIDStrs))

		parsedID, err := uuid.Parse(ibpIDStr)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid InfiniBand Partition ID: %s in query", ibpIDStr), nil)
		}
		infiniBandPartitionIDs = append(infiniBandPartitionIDs, parsedID)
	}

	if len(ibpIDStrs) > 0 {
		// Set tracer span attribute
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("infinibandPartitionIds", ibpIDStrs))

		// Deduplicate InfiniBand Partition IDs
		infiniBandPartitionIDs = goset.NewSet(infiniBandPartitionIDs...).ToSlice()

		ibpDAO := cdbm.NewInfiniBandPartitionDAO(gaibih.dbSession)
		ibPartitions, _, err := ibpDAO.GetAll(ctx, nil, cdbm.InfiniBandPartitionFilterInput{InfiniBandPartitionIDs: infiniBandPartitionIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving InfiniBand Partitions from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Partitions", nil)
		}

		ibPartitionIDMap := map[uuid.UUID]*cdbm.InfiniBandPartition{}
		for i := range ibPartitions {
			ibPartitionIDMap[ibPartitions[i].ID] = &ibPartitions[i]
		}

		for _, ibPartitionID := range infiniBandPartitionIDs {
			ibPartition, ok := ibPartitionIDMap[ibPartitionID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find InfiniBand Partition with ID: %s specified in query", ibPartitionID.String()), nil)
			}

			if ibPartition.TenantID != tenant.ID {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("InfiniBand Partition: %s specified in query doesn't belong to current Tenant", ibPartitionID.String()), nil)
			}
		}
	}

	// Get status from query param
	var statuses []string
	qStatuses := qParams["status"]
	for _, status := range qStatuses {
		cotel.SetAttribute(handlerSpan, attribute.String("status", status))
		_, ok := cdbm.InfiniBandInterfaceStatusMap[status]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", status))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Status value: %s in query", status), nil)
		}
		statuses = append(statuses, status)
	}

	if len(qStatuses) > 0 {
		// Set tracer span attribute
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("statuses", qStatuses))
	}

	// Get the InfiniBand Interfaces record from the db
	ibIfcDAO := cdbm.NewInfiniBandInterfaceDAO(gaibih.dbSession)

	filterInput := cdbm.InfiniBandInterfaceFilterInput{
		SiteIDs:                siteIDs,
		InstanceIDs:            instanceIDs,
		InfiniBandPartitionIDs: infiniBandPartitionIDs,
		Statuses:               statuses,
	}

	pageInput := cdbp.PageInput{
		Limit:   pageRequest.Limit,
		Offset:  pageRequest.Offset,
		OrderBy: pageRequest.OrderBy,
	}

	dbIbInterfaces, total, err := ibIfcDAO.GetAll(ctx, nil, filterInput, pageInput, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving InfiniBand Interface Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Interfaces, DB error", nil)
	}

	// Create response
	apiIbInterfaces := []model.APIInfiniBandInterface{}
	for i := range dbIbInterfaces {
		apiIbInterfaces = append(apiIbInterfaces, *model.NewAPIInfiniBandInterface(&dbIbInterfaces[i]))
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiIbInterfaces)
}
