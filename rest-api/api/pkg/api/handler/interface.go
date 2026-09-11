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

// ~~~~~ GetAll Interface Handler ~~~~~ //

// GetAllInterfaceHandler is the API Handler for retrieving all Interfaces for an Instance
type GetAllInterfaceHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllInterfaceHandler initializes and returns a new handler for retrieving all subnets for an Instance
func NewGetAllInterfaceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllInterfaceHandler {
	return GetAllInterfaceHandler{
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

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
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

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		cotel.SetAttribute(handlerSpan, attribute.String("status", statusQuery))
		_, ok := cdbm.InterfaceStatusMap[statusQuery]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		status = &statusQuery
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

	// Get Instance ID from URL param
	instanceStrID := c.Param("instanceId")

	// Get Instance
	instance, err := common.GetInstanceFromIDString(ctx, nil, instanceStrID, gaish.dbSession)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Instance with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance ID in URL", nil)
	}

	// Check if Instance belongs to Tenant
	if instance.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance does not belong to current Tenant", nil)
	}

	instanceID := instance.ID

	// Get the instance subnets record from the db
	ifcDAO := cdbm.NewInterfaceDAO(gaish.dbSession)

	filterInput := cdbm.InterfaceFilterInput{
		InstanceIDs: []uuid.UUID{instanceID},
	}

	if status != nil {
		filterInput.Statuses = append(filterInput.Statuses, *status)
	}

	pageInput := paginator.PageInput{
		Limit:   pageRequest.Limit,
		Offset:  pageRequest.Offset,
		OrderBy: pageRequest.OrderBy,
	}

	dbInterfaces, total, err := ifcDAO.GetAll(ctx, nil, filterInput, pageInput, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instance Subnet Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Subnet Details for Instance", nil)
	}

	// Create response
	apiInterfaces := []model.APIInterface{}
	for _, dbins := range dbInterfaces {
		isubnets := model.NewAPIInterface(&dbins)
		apiInterfaces = append(apiInterfaces, *isubnets)
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
