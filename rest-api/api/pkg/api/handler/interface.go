// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"fmt"
	"net/http"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"

	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
)

// GetAllInstanceInterfaceHandler is the API Handler for retrieving all Interfaces for an Instance.
type GetAllInstanceInterfaceHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllInstanceInterfaceHandler initializes an Instance-scoped Interface handler.
func NewGetAllInstanceInterfaceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllInstanceInterfaceHandler {
	return GetAllInstanceInterfaceHandler{
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
func (h GetAllInstanceInterfaceHandler) Handle(c echo.Context) error {
	instanceID := c.Param("instanceId")
	delegate := NewGetAllInterfaceHandler(h.dbSession, h.tc, h.cfg, &common.QueryOverride{
		InstanceIDs:   []string{instanceID},
		ValueFromPath: true,
	})
	return delegate.Handle(c)
}

// ~~~~~ GetAll Interface Handler ~~~~~ //

// GetAllInterfaceHandler is the API Handler for retrieving Tenant-visible Interfaces.
type GetAllInterfaceHandler struct {
	dbSession     *cdb.Session
	tc            temporalClient.Client
	cfg           *config.Config
	queryOverride *common.QueryOverride
}

// NewGetAllInterfaceHandler initializes an Interface handler. A query override supplies
// path-scoped values when the Instance endpoint delegates to the general endpoint.
func NewGetAllInterfaceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config, queryOverride ...*common.QueryOverride) GetAllInterfaceHandler {
	var override *common.QueryOverride
	if len(queryOverride) > 0 {
		override = queryOverride[0]
	}
	return GetAllInterfaceHandler{
		dbSession:     dbSession,
		tc:            tc,
		cfg:           cfg,
		queryOverride: override,
	}
}

// Handle godoc
// @Summary Retrieve Tenant-visible Interfaces
// @Description Retrieve Interfaces visible to the current Tenant
// @Tags Interface
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param vpcId query string false "Filter by attached VPC ID"
// @Param crossTenant query boolean false "Filter by whether the Instance and selected VPC or VPC Prefix have different owning Tenants"
// @Param instanceTenantId query string false "Filter by Instance owning Tenant ID"
// @Param vpcTenantId query string false "Filter by selected VPC or VPC Prefix owning Tenant ID"
// @Param status query string false "Filter by status"
// @Param includeRelation query string false "Related entities to include in response"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} model.APIInterface
// @Router /v2/org/{org}/nico/interface [get]
func (gaish GetAllInterfaceHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Interface", "GetAll", c)
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

	request := model.APIInterfaceGetAllRequest{}
	pageRequest := pagination.PageRequest{}
	instanceIDFromPath := gaish.queryOverride != nil && gaish.queryOverride.ValueFromPath
	knownQueryRequest := any(request)
	if instanceIDFromPath {
		knownQueryRequest = struct {
			Status          *string  `query:"status"`
			IncludeRelation []string `query:"includeRelation"`
		}{}
	}
	err = common.ValidateKnownQueryParams(c.QueryParams(), knownQueryRequest, pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&request)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding Interface filters")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse Interface filters", nil)
	}
	err = request.Validate()
	if err != nil {
		logger.Warn().Err(err).Msg("error validating Interface filters")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Interface filters", err)
	}

	// Validate pagination request
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination request attributes
	err = pageRequest.Validate(cdbm.InterfaceOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InterfaceRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get status from query param
	var status *string

	if request.Status != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("status", *request.Status))
		_, ok := cdbm.InterfaceStatusMap[*request.Status]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", *request.Status))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		status = request.Status
	}

	// Get Tenant for this org
	tenant, err := common.GetTenantForOrg(ctx, nil, gaish.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	instanceIDStrs := []string{}
	if instanceIDFromPath {
		instanceIDStrs = gaish.queryOverride.InstanceIDs
	}
	var instanceIDs []uuid.UUID
	for _, instanceIDStr := range instanceIDStrs {
		instanceID, parseErr := uuid.Parse(instanceIDStr)
		if parseErr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Instance ID: %s specified in request", instanceIDStr), nil)
		}
		instanceIDs = append(instanceIDs, instanceID)
	}
	if len(instanceIDs) > 0 {
		instances, _, getErr := cdbm.NewInstanceDAO(gaish.dbSession).GetAll(ctx, nil, cdbm.InstanceFilterInput{
			InstanceIDs: instanceIDs,
		}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if getErr != nil {
			logger.Error().Err(getErr).Msg("error retrieving Instances from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance specified in request", nil)
		}
		instancesByID := make(map[uuid.UUID]cdbm.Instance, len(instances))
		for _, instance := range instances {
			instancesByID[instance.ID] = instance
		}
		for _, instanceID := range instanceIDs {
			instance, ok := instancesByID[instanceID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Instance with ID: %s specified in request", instanceID), nil)
			}
			if instance.TenantID != tenant.ID {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Instance: %s specified in request doesn't belong to current Tenant", instanceID), nil)
			}
		}
	}

	filterInput := cdbm.InterfaceFilterInput{InstanceIDs: instanceIDs}
	if !instanceIDFromPath {
		filterInput.VisibleToTenantID = &tenant.ID
		filterInput.CrossTenant = request.CrossTenant
		if request.VpcID != nil {
			vpcID := uuid.MustParse(*request.VpcID)
			filterInput.VpcID = &vpcID
		}
		if request.InstanceTenantID != nil {
			instanceTenantID := uuid.MustParse(*request.InstanceTenantID)
			filterInput.InstanceTenantID = &instanceTenantID
		}
		if request.VpcTenantID != nil {
			vpcTenantID := uuid.MustParse(*request.VpcTenantID)
			filterInput.VpcTenantID = &vpcTenantID
		}
	}

	if status != nil {
		filterInput.Statuses = append(filterInput.Statuses, *status)
	}

	pageInput := paginator.PageInput{
		Limit:   pageRequest.Limit,
		Offset:  pageRequest.Offset,
		OrderBy: pageRequest.OrderBy,
	}

	requestedRelations := make(map[string]bool, len(qIncludeRelations))
	for _, relation := range qIncludeRelations {
		requestedRelations[relation] = true
	}
	queryRelations := append([]string{}, qIncludeRelations...)
	for _, relation := range []string{cdbm.InstanceRelationName, cdbm.VpcRelationName, cdbm.VpcPrefixRelationName} {
		if !requestedRelations[relation] {
			queryRelations = append(queryRelations, relation)
		}
	}

	dbInterfaces, total, err := cdbm.NewInterfaceDAO(gaish.dbSession).GetAll(ctx, nil, filterInput, pageInput, queryRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Interfaces from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Interfaces", nil)
	}

	// Create response
	apiInterfaces := make([]model.APIInterface, 0, len(dbInterfaces))
	for i := range dbInterfaces {
		dbInterface := &dbInterfaces[i]
		if dbInterface.Instance == nil {
			logger.Error().Str("interfaceID", dbInterface.ID.String()).Msg("Interface is missing its Instance relation")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to build Interface response", nil)
		}

		switch {
		case dbInterface.VpcID != nil:
			if dbInterface.Vpc == nil {
				logger.Error().Str("interfaceID", dbInterface.ID.String()).Msg("Interface is missing its VPC relation")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to build Interface response", nil)
			}
		case dbInterface.VpcPrefixID != nil:
			if dbInterface.VpcPrefix == nil {
				logger.Error().Str("interfaceID", dbInterface.ID.String()).Msg("Interface is missing its VPC Prefix relation")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to build Interface response", nil)
			}
		}

		apiInterface := model.NewAPIInterface(dbInterface)
		if !requestedRelations[cdbm.InstanceRelationName] {
			apiInterface.Instance = nil
		}
		if !requestedRelations[cdbm.VpcPrefixRelationName] {
			apiInterface.VpcPrefix = nil
		}
		apiInterfaces = append(apiInterfaces, *apiInterface)
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

	return c.JSON(http.StatusOK, apiInterfaces)
}
