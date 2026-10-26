// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"fmt"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// GetAllInterfaceSummaryHandler retrieves the Tenant-visible, owner-safe Interface view.
type GetAllInterfaceSummaryHandler struct {
	dbSession *cdb.Session
}

// NewGetAllInterfaceSummaryHandler initializes an Interface summary list handler.
func NewGetAllInterfaceSummaryHandler(dbSession *cdb.Session) GetAllInterfaceSummaryHandler {
	return GetAllInterfaceSummaryHandler{dbSession: dbSession}
}

// Handle godoc
// @Summary Retrieve Tenant-visible Interface summaries
// @Description Retrieve VPC-backed Interface summaries visible to the current Tenant
// @Tags interface
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param vpcId query string false "Filter by attached VPC ID"
// @Param crossTenant query boolean false "Filter by whether the Instance belongs to another Tenant"
// @Param instanceTenantId query string false "Filter by consuming Instance Tenant ID"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} model.APIInterfaceSummary
// @Router /v2/org/{org}/nico/interface [get]
func (h GetAllInterfaceSummaryHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InterfaceSummary", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	request := model.APIInterfaceSummaryGetAllRequest{}
	pageRequest := pagination.PageRequest{}
	err = common.ValidateKnownQueryParams(c.QueryParams(), request, pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&request)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding Interface summary filters")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse Interface summary filters", nil)
	}
	err = request.Validate()
	if err != nil {
		logger.Warn().Err(err).Msg("error validating Interface summary filters")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Interface summary filters", err)
	}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	err = pageRequest.Validate(cdbm.InterfaceOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	tenant, err := common.GetTenantForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	filter := cdbm.InterfaceFilterInput{
		VisibleToTenantID: &tenant.ID,
		CrossTenant:       request.CrossTenant,
		VpcAttachedOnly:   true,
	}
	if request.VpcID != nil {
		vpcID := uuid.MustParse(*request.VpcID)
		filter.VpcID = &vpcID
	}
	if request.InstanceTenantID != nil {
		instanceTenantID := uuid.MustParse(*request.InstanceTenantID)
		filter.InstanceTenantID = &instanceTenantID
	}

	dbInterfaces, total, err := cdbm.NewInterfaceDAO(h.dbSession).GetAll(
		ctx,
		nil,
		filter,
		cdbp.PageInput{
			Limit:   pageRequest.Limit,
			Offset:  pageRequest.Offset,
			OrderBy: pageRequest.OrderBy,
		},
		[]string{cdbm.InstanceRelationName, cdbm.VpcPrefixRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Interface summaries from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Interface summaries", nil)
	}

	summaries := make([]model.APIInterfaceSummary, 0, len(dbInterfaces))
	for i := range dbInterfaces {
		if dbInterfaces[i].Instance == nil {
			logger.Error().Str("interfaceID", dbInterfaces[i].ID.String()).Msg("Interface summary is missing its Instance relation")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to build Interface summary response", nil)
		}
		if dbInterfaces[i].VpcID == nil && dbInterfaces[i].VpcPrefix == nil {
			logger.Error().Str("interfaceID", dbInterfaces[i].ID.String()).Msg("Interface summary is missing its VPC relation")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to build Interface summary response", nil)
		}
		var summary model.APIInterfaceSummary
		summary.FromDBModel(&dbInterfaces[i])
		summaries = append(summaries, summary)
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	return c.JSON(http.StatusOK, summaries)
}
