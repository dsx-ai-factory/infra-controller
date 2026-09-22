// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"fmt"
	"net/http"

	temporalClient "go.temporal.io/sdk/client"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateInfrastructureProviderHandler is the API Handler for creating new Infrastructure Provider
type CreateInfrastructureProviderHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateInfrastructureProviderHandler initializes and returns a new handler for creating Infrastructure Provider
func NewCreateInfrastructureProviderHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) CreateInfrastructureProviderHandler {
	return CreateInfrastructureProviderHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create an Infrastructure Provider for the org
// @Description Create an Infrastructure Provider for the org. Only one Infrastructure Provider is allowed per org.
// @Tags infrastructureprovider
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIInfrastructureProviderCreateRequest true "Infrastructure Provider create request"
// @Success 201 {object} model.APIInfrastructureProvider
// @Router /v2/org/{org}/nico/infrastructure-provider [post]
func (ciph CreateInfrastructureProviderHandler) Handle(c echo.Context) error {
	org, dbUser, _, logger, handlerSpan := common.SetupHandler("InfrastructureProvider", "Create", c, ciph.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to interact with Infrastructure Provider endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, model.ErrMsgproviderCreateEndpointDeprecated, nil)
}

// ~~~~~ Get Current Handler ~~~~~ //

// GetCurrentInfrastructureProviderHandler is the API Handler for retrieving Infrastructure Provider associated with the org
type GetCurrentInfrastructureProviderHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetCurrentInfrastructureProviderHandler initializes and returns a new handler to retrieve Infrastructure Provider associate with the org
func NewGetCurrentInfrastructureProviderHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetCurrentInfrastructureProviderHandler {
	return GetCurrentInfrastructureProviderHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the Infrastructure Provider associated with the org
// @Description Retrieve the Infrastructure Provider associated with the org. If it does not exist, it will be created.
// @Tags infrastructureprovider
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APIInfrastructureProvider
// @Router /v2/org/{org}/nico/infrastructure-provider/current [get]
func (gciph GetCurrentInfrastructureProviderHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfrastructureProvider", "GetCurrent", c, gciph.tracerSpan)
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
	userOrgDetails, _ := dbUser.OrgData.GetOrgByName(org)

	// Validate role, only Provider Admins are allowed to interact with Infrastructure Provider endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole, auth.ProviderViewerRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for this org
	ipDAO := cdbm.NewInfrastructureProviderDAO(gciph.dbSession)

	var ip *cdbm.InfrastructureProvider

	ips, err := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Infrastructure Provider for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider", nil)
	}

	var serr error
	if len(ips) == 0 {
		// Create Infrastructure Provider
		ip, serr = ipDAO.Create(ctx, nil, cdbm.InfrastructureProviderCreateInput{
			Name:           userOrgDetails.Name,
			Org:            org,
			OrgDisplayName: cutil.GetPtr(userOrgDetails.DisplayName),
			CreatedBy:      dbUser.ID,
		})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Infrastructure Provider DB entity")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider", nil)
		}
	} else {
		ip = &ips[0]
		if ip.OrgDisplayName == nil || *ip.OrgDisplayName != userOrgDetails.DisplayName {
			ip, serr = ipDAO.Update(ctx, nil, cdbm.InfrastructureProviderUpdateInput{
				InfrastructureProviderID: ip.ID,
				OrgDisplayName:           cutil.GetPtr(userOrgDetails.DisplayName),
			})
			if serr != nil {
				logger.Error().Err(serr).Msg("error updating Infrastructure Provider DB entity")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider", nil)
			}
		}
	}

	// Create response
	apiInstance := model.NewAPIInfrastructureProvider(ip)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Get Current Stats Handler ~~~~~ //

// GetCurrentInfrastructureProviderStatsHandler is the API Handler for retrieving InfrastructureProvider stats associated with the org
type GetCurrentInfrastructureProviderStatsHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetCurrentInfrastructureProviderStatsHandler initializes and returns a new handler to retrieve InfrastructureProvider stats associate with the org
func NewGetCurrentInfrastructureProviderStatsHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetCurrentInfrastructureProviderStatsHandler {
	return GetCurrentInfrastructureProviderStatsHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the Infrastructure Provider stats associated with the org
// @Description Retrieve the Infrastructure Provider stats associated with the org
// @Tags infrastructureprovider
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APIInfrastructureProviderStats
// @Router /v2/org/{org}/nico/infrastructure-provider/current/stats [get]
func (gcipsh GetCurrentInfrastructureProviderStatsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfrastructureProvider", "GetCurrentStats", c, gcipsh.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to interact with Infrastructure Provider endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for this org
	ipDAO := cdbm.NewInfrastructureProviderDAO(gcipsh.dbSession)

	ips, err := ipDAO.GetAllByOrg(ctx, nil, org, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Infrastructure Provider for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider", nil)
	}

	if len(ips) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound,
			fmt.Sprintf("Org '%v' does not have an Infrastructure Provider", org), nil)
	}

	// Get Machine stats for this org infrastructure provider
	mcDAO := cdbm.NewMachineDAO(gcipsh.dbSession)
	mcStatsMap, err := mcDAO.GetCountByStatus(ctx, nil, cutil.GetPtr(ips[0].ID), nil, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Machine stats for this org's infrastructure provider")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine stats", nil)
	}

	// Get IPBlock stats for this org infrastructure provider
	ipbDAO := cdbm.NewIPBlockDAO(gcipsh.dbSession)
	ipbFilter := cdbm.IPBlockFilterInput{}
	ipbFilter.ProviderVisible(ips[0].ID)
	ipbStatsMap, err := ipbDAO.GetCountByStatus(ctx, nil, ipbFilter)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving IPBlock stats for this org's infrastructure provider")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IPBlock stats", nil)
	}

	// Get TenantAccount stats for this org infrastructure provider
	taDAO := cdbm.NewTenantAccountDAO(gcipsh.dbSession)
	taStatsMap, err := taDAO.GetCountByStatus(ctx, nil, cutil.GetPtr(ips[0].ID), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving TenantAccount stats for this org's infrastructure provider")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate Tenant Account stats for Infrastructure Provider", nil)
	}

	// Create response
	apiInfrastructureProviderStats := model.NewAPIInfrastructureProviderStats(mcStatsMap, ipbStatsMap, taStatsMap)
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiInfrastructureProviderStats)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateCurrentInfrastructureProviderHandler is the API Handler for updating the current Infrastructure Provider
type UpdateCurrentInfrastructureProviderHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateCurrentInfrastructureProviderHandler initializes and returns a new handler for updating the current Infrastructure Provider
func NewUpdateCurrentInfrastructureProviderHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateCurrentInfrastructureProviderHandler {
	return UpdateCurrentInfrastructureProviderHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing Infrastructure Provider
// @Description Update an existing Infrastructure Provider for the org
// @Tags infrastructureprovider
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIInfrastructureProviderUpdateRequest true "Infrastructure Provider update request"
// @Success 200 {object} model.APIInfrastructureProvider
// @Router /v2/org/{org}/nico/infrastructure-provider/current [patch]
func (uciph UpdateCurrentInfrastructureProviderHandler) Handle(c echo.Context) error {
	org, dbUser, _, logger, handlerSpan := common.SetupHandler("InfrastructureProvider", "UpdateCurrent", c, uciph.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to interact with Infrastructure Provider endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, model.ErrMsgproviderUpdateEndpointDeprecated, nil)
}
