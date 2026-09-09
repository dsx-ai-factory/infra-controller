// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// GetAllMachineCapabilityHandler is an API Handler to return various Machine Capabilities
type GetAllMachineCapabilityHandler struct {
	dbSession *cdb.Session
}

// NewGetAllMachineCapabilityHandler creates and returns a new handler for retrieving Machine Capabilities
func NewGetAllMachineCapabilityHandler(dbSession *cdb.Session) GetAllMachineCapabilityHandler {
	return GetAllMachineCapabilityHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Return information about the current user
// @Description Get basic information about the user making the request
// @Tags user
// @Accept */*
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param site_id query string true "Filter by site ID"
// @Success 200 {object} model.APIUser
// @Router /v2/org/{org}/nico/machine-capability [get]
func (gamch GetAllMachineCapabilityHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineCapability", "GetAll", c)
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

	// Validate role, only Provider Admins are allowed to retrieve Machine/InstanceType associations
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate paginantion request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.MachineCapabilityOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Validate other query params
	orgInfrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, gamch.dbSession, org)
	if err != nil {
		if err == common.ErrOrgInstrastructureProviderNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Infrastructure Provider not found in org", nil)
		}
		logger.Error().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve infrastructure provider for org, DB error", nil)
	}

	mDAO := cdbm.NewMachineDAO(gamch.dbSession)

	// Get and validate query params
	qSiteID := c.QueryParam("siteId")

	// Validate site id if provided
	var site *cdbm.Site
	if qSiteID != "" {
		var serr error
		site, serr = common.GetSiteFromIDString(ctx, nil, qSiteID, gamch.dbSession)
		if serr != nil {
			if serr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Site specified in query", nil)
			}
			logger.Error().Err(serr).Str("Site ID", qSiteID).Msg("error retrieving Site specified in query")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Error retrieving Site specified in query", nil)
		}

		if site.InfrastructureProviderID != orgInfrastructureProvider.ID {
			logger.Error().Msg("Site's Infrastructure Provider doesn't match org's Infrastructure Provider")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in query doesn't belong to org's Infrastructure provider", nil)
		}
	}

	// Check if `hasInstanceType` query params
	var hasInstanceType *bool
	qHasInstanceType := c.QueryParam("hasInstanceType")
	if qHasInstanceType != "" {
		hiType, serr := strconv.ParseBool(qHasInstanceType)
		if serr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid value: %v specified for hasInstanceType in query", qHasInstanceType), nil)
		}

		hasInstanceType = cutil.GetPtr(hiType)
	}

	// Get Machines
	filterInput := cdbm.MachineFilterInput{
		InfrastructureProviderIDs: []uuid.UUID{orgInfrastructureProvider.ID},
		HasInstanceType:           hasInstanceType,
		ExcludeMetadata:           true, // Exclude metadata since we're only retrieving Machines for the IDs
	}
	if site != nil {
		filterInput.SiteIDs = []uuid.UUID{site.ID}
	}

	ms, _, err := mDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error getting Machines from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machines, DB error", nil)
	}

	mids := []string{}
	for _, m := range ms {
		mids = append(mids, m.ID)
	}

	// Get other Machine Capability filters
	var capabilityType, name, frequency, capacity, vendor *string
	var count *int
	var deviceType *string
	var inactiveDevices []int

	capabilityTypeParam := c.QueryParam("type")
	if capabilityTypeParam != "" {
		capabilityType = &capabilityTypeParam
	}

	nameParam := c.QueryParam("name")
	if nameParam != "" {
		name = &nameParam
	}

	frequencyParam := c.QueryParam("frequency")
	if frequencyParam != "" {
		frequency = &frequencyParam
	}

	capacityParam := c.QueryParam("capacity")
	if capacityParam != "" {
		capacity = &capacityParam
	}

	vendorParam := c.QueryParam("vendor")
	if vendorParam != "" {
		vendor = &vendorParam
	}

	countParam := c.QueryParam("count")
	if countParam != "" {
		countVal, serr := strconv.Atoi(countParam)
		if serr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for count in query", nil)
		}
		count = &countVal
	}

	deviceTypeParam := c.QueryParam("devicetype")
	if deviceTypeParam != "" {
		deviceType = &deviceTypeParam
	}

	inactiveDevicesParam := c.QueryParams()["inactiveDevices"]
	if len(inactiveDevicesParam) > 0 {
		for _, inactiveDeviceStr := range inactiveDevicesParam {
			inactiveDeviceInt, serr := strconv.Atoi(inactiveDeviceStr)
			if serr != nil {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for inactiveDevices in query", nil)
			}
			inactiveDevices = append(inactiveDevices, inactiveDeviceInt)
		}
	}

	// Get Machine Capabilities
	mcDAO := cdbm.NewMachineCapabilityDAO(gamch.dbSession)
	mcs, total, err := mcDAO.GetAllDistinct(ctx, nil, mids, nil, capabilityType, name, frequency, capacity, vendor, count, deviceType, inactiveDevices, pageRequest.Offset, pageRequest.Limit, pageRequest.OrderBy)
	if err != nil {
		logger.Error().Err(err).Msg("error getting Machine Capabilities from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine Capabilities, DB error", nil)
	}

	// Return response
	amcs := []*model.APIMachineCapability{}

	for _, mc := range mcs {
		amcs = append(amcs, model.NewAPIMachineCapability(&mc))
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

	return c.JSON(http.StatusOK, amcs)
}
