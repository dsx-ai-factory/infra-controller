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

// ~~~~~ GetAll Instance NVLinkInterface Handler ~~~~~ //

// GetAllInstanceNVLinkInterfaceHandler is the API Handler for retrieving all NVLinkInterfaces for an Instance
type GetAllInstanceNVLinkInterfaceHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllInstanceNVLinkInterfaceHandler initializes and returns a new handler for retrieving all NVLinkInterfaces for an Instance
func NewGetAllInstanceNVLinkInterfaceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllInstanceNVLinkInterfaceHandler {
	return GetAllInstanceNVLinkInterfaceHandler{
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
func (ganvliih GetAllInstanceNVLinkInterfaceHandler) Handle(c echo.Context) error {
	instanceID := c.Param("instanceId")
	queryOverride := &common.QueryOverride{
		InstanceIDs:   []string{instanceID},
		ValueFromPath: true,
	}
	delegate := NewGetAllNVLinkInterfaceHandler(ganvliih.dbSession, ganvliih.tc, ganvliih.cfg, queryOverride)
	return delegate.Handle(c)
}

// ~~~~~ GetAll NVLinkInterface Handler ~~~~~ //

// GetAllNVLinkInterfaceHandler is the API Handler for retrieving all NVLinkInterfaces
type GetAllNVLinkInterfaceHandler struct {
	dbSession     *cdb.Session
	tc            temporalClient.Client
	cfg           *config.Config
	queryOverride *common.QueryOverride
}

// NewGetAllNVLinkInterfaceHandler initializes and returns a new handler for retrieving all NVLinkInterfaces.
// When queryOverride is provided (e.g. when delegating from instance-scoped endpoint), it supplies values
// that would otherwise come from query params, and error messages use "request" instead of "query".
func NewGetAllNVLinkInterfaceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config, queryOverride ...*common.QueryOverride) GetAllNVLinkInterfaceHandler {
	var override *common.QueryOverride
	if len(queryOverride) > 0 {
		override = queryOverride[0]
	}
	return GetAllNVLinkInterfaceHandler{
		dbSession:     dbSession,
		tc:            tc,
		cfg:           cfg,
		queryOverride: override,
	}
}

// Handle godoc
// @Summary Retrieve all NVLinkInterfaces
// @Description Retrieve all NVLinkInterfaces
// @Tags NVLinkInterface
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param instanceId path string true "ID of Instance"
// @Param nvlinkLogicalPartitionId path string true "ID of NVLinkLogicalPartition"
// @Param nvLinkDomainId path string true "ID of NVLinkDomain"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param includeRelation query string false "Related entities to include in response e.g. 'NVLinkLogicalPartition, Instance'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} model.APINVLinkInterface
// @Router /v2/org/{org}/nico/nvlink-interface [get]
func (gaish GetAllNVLinkInterfaceHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkInterface", "GetAll", c)
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
	err = pageRequest.Validate(cdbm.NVLinkInterfaceOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.NVLinkInterfaceRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gaish.dbSession)

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
		tsDAO := cdbm.NewTenantSiteDAO(gaish.dbSession)
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
	instanceIDFromPath := gaish.queryOverride != nil && gaish.queryOverride.ValueFromPath

	instanceIDStrs := qParams["instanceId"]
	if instanceIDFromPath && len(gaish.queryOverride.InstanceIDs) > 0 {
		instanceIDStrs = gaish.queryOverride.InstanceIDs
	}

	for _, instanceIDStr := range instanceIDStrs {
		parsedID, err := uuid.Parse(instanceIDStr)
		if err != nil {
			if instanceIDFromPath {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Instance ID: %s specified in request", instanceIDStr), nil)
			}
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Instance ID: %s in query", instanceIDStr), nil)
		}
		instanceIDs = append(instanceIDs, parsedID)
	}

	if len(instanceIDStrs) > 0 {
		// Set tracer span attribute
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("instanceIds", instanceIDStrs))

		// De-duplicate Instance IDs
		instanceIDs = goset.NewSet(instanceIDs...).ToSlice()

		instanceDAO := cdbm.NewInstanceDAO(gaish.dbSession)
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

	// Get NVLink Logical Partition IDs from query param - parse first, then bulk fetch
	var nvlinkLogicalPartitionIDs []uuid.UUID
	nvllpIDStrs := qParams["nvLinkLogicalPartitionId"]

	for _, nvllpIDStr := range nvllpIDStrs {
		parsedID, err := uuid.Parse(nvllpIDStr)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid NVLink Logical Partition ID: %s in query", nvllpIDStr), nil)
		}
		nvlinkLogicalPartitionIDs = append(nvlinkLogicalPartitionIDs, parsedID)
	}

	if len(nvllpIDStrs) > 0 {
		// Set tracer span attribute
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("nvLinkLogicalPartitionIds", nvllpIDStrs))

		// Deduplicate NVLink Logical Partition IDs
		nvlinkLogicalPartitionIDs = goset.NewSet(nvlinkLogicalPartitionIDs...).ToSlice()

		nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(gaish.dbSession)
		nvllps, _, err := nvllpDAO.GetAll(ctx, nil, cdbm.NVLinkLogicalPartitionFilterInput{NVLinkLogicalPartitionIDs: nvlinkLogicalPartitionIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLink Logical Partitions from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partitions specified in query, DB error", nil)
		}

		nvllpIDMap := map[uuid.UUID]*cdbm.NVLinkLogicalPartition{}
		for i := range nvllps {
			nvllpIDMap[nvllps[i].ID] = &nvllps[i]
		}

		for _, nvllpID := range nvlinkLogicalPartitionIDs {
			nvllp, ok := nvllpIDMap[nvllpID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find NVLink Logical Partition with ID: %s specified in query", nvllpID.String()), nil)
			}

			if nvllp.TenantID != tenant.ID {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("NVLink Logical Partition: %s specified in query doesn't belong to current Tenant", nvllpID.String()), nil)
			}
		}
	}

	// Get NVLink Domain IDs from query param - parse first, then deduplicate
	var nvLinkDomainIDs []uuid.UUID
	nvLinkDomainIDStrs := qParams["nvLinkDomainId"]

	for _, nvLinkDomainIDStr := range nvLinkDomainIDStrs {
		parsedID, err := uuid.Parse(nvLinkDomainIDStr)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid NVLink Domain ID: %s in query", nvLinkDomainIDStr), nil)
		}
		nvLinkDomainIDs = append(nvLinkDomainIDs, parsedID)
	}

	if len(nvLinkDomainIDStrs) > 0 {
		// Set tracer span attribute
		cotel.SetAttribute(handlerSpan, attribute.StringSlice("nvLinkDomainIds", nvLinkDomainIDStrs))

		// Deduplicate NVLink Domain IDs
		nvLinkDomainIDs = goset.NewSet(nvLinkDomainIDs...).ToSlice()
	}

	// Get status from query param
	var statuses []string
	qStatuses := qParams["status"]
	for _, status := range qStatuses {
		cotel.SetAttribute(handlerSpan, attribute.String("status", status))
		_, ok := cdbm.NVLinkInterfaceStatusMap[status]
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

	// Get the NVLink Logical Partition NVLink Interfaces record from the db
	nvlIfcDAO := cdbm.NewNVLinkInterfaceDAO(gaish.dbSession)

	filterInput := cdbm.NVLinkInterfaceFilterInput{
		SiteIDs:                   siteIDs,
		InstanceIDs:               instanceIDs,
		NVLinkLogicalPartitionIDs: nvlinkLogicalPartitionIDs,
		NVLinkDomainIDs:           nvLinkDomainIDs,
		Statuses:                  statuses,
	}

	pageInput := cdbp.PageInput{
		Limit:   pageRequest.Limit,
		Offset:  pageRequest.Offset,
		OrderBy: pageRequest.OrderBy,
	}

	dbNVLinkInterfaces, total, err := nvlIfcDAO.GetAll(ctx, nil, filterInput, pageInput, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving NVLink Interface Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Interfaces, DB error", nil)
	}

	// Create response
	apiNVLinkInterfaces := []model.APINVLinkInterface{}
	for _, dbnvlifc := range dbNVLinkInterfaces {
		curnvlifc := dbnvlifc
		apiNVLinkInterfaces = append(apiNVLinkInterfaces, *model.NewAPINVLinkInterface(&curnvlifc))
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

	return c.JSON(http.StatusOK, apiNVLinkInterfaces)
}
