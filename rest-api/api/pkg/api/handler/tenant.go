// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"fmt"
	"net/http"

	"github.com/google/uuid"
	temporalClient "go.temporal.io/sdk/client"

	"github.com/rs/zerolog"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateTenantHandler is the API Handler for creating new Tenant
type CreateTenantHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateTenantHandler initializes and returns a new handler for creating Tenant
func NewCreateTenantHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) CreateTenantHandler {
	return CreateTenantHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create a Tenant for the org
// @Description Create a Tenant for the org. Only one Tenant is allowed per org.
// @Tags tenant
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APITenantCreateRequest true "Tenant creation request"
// @Success 201 {object} model.APITenant
// @Router /v2/org/{org}/nico/tenant [post]
func (cth CreateTenantHandler) Handle(c echo.Context) error {
	org, dbUser, _, logger, handlerSpan := common.SetupHandler("Tenant", "Create", c, cth.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Tenant endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, model.ErrMsgTenantCreateEndpointDeprecated, nil)
}

// ~~~~~ Get Current Handler ~~~~~ //

// GetCurrentTenantHandler is the API Handler for retrieving Tenant associated with the org
type GetCurrentTenantHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetCurrentTenantHandler initializes and returns a new handler to retrieve Tenant associate with the org
func NewGetCurrentTenantHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetCurrentTenantHandler {
	return GetCurrentTenantHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the Tenant associated with the org
// @Description Retrieve the Tenant associated with the org. If it does not exist, it will be created.
// @Tags tenant
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APITenant
// @Router /v2/org/{org}/nico/tenant/current [get]
func (gcth GetCurrentTenantHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tenant", "GetCurrent", c, gcth.tracerSpan)
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

	//Validate role, only Tenant Admins are allowed to interact with Tenant endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gcth.dbSession)

	tn, err := cdb.WithTxResult(ctx, gcth.dbSession, func(tx *cdb.Tx) (*cdbm.Tenant, error) {
		// Acquire an advisory lock on the org to serialize concurrent
		// get-or-create attempts against the same Tenant.
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(org), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to acquire advisory lock for Tenant get-or-create")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current Tenant, unable to acquire lock", nil)
		}

		// Re-read inside the tx so the existence check and any create/update
		// happen against the same locked snapshot.
		tns, _, derr := tnDAO.GetAll(ctx, tx, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Tenant for this org")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current Tenant", nil)
		}

		if len(tns) == 0 {
			// Create Tenant
			created, derr := tnDAO.Create(ctx, tx, cdbm.TenantCreateInput{
				Name:           userOrgDetails.Name,
				DisplayName:    &userOrgDetails.DisplayName,
				Org:            org,
				OrgDisplayName: cutil.GetPtr(org),
				CreatedBy:      dbUser.ID,
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Tenant DB entity")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
			}

			// Update Tenant Accounts if needed
			derr = updateTenantAccounts(ctx, gcth.dbSession, tx, logger, created)
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating Tenant Accounts")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Tenant, could not update Tenant Accounts", nil)
			}
			return created, nil
		}

		// Update Tenant if needed
		existing := &tns[0]
		if existing.OrgDisplayName == nil || *existing.OrgDisplayName != userOrgDetails.DisplayName {
			updated, derr := tnDAO.Update(ctx, tx, cdbm.TenantUpdateInput{
				TenantID:       existing.ID,
				OrgDisplayName: cutil.GetPtr(userOrgDetails.DisplayName),
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating Tenant DB entity")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
			}
			return updated, nil
		}
		return existing, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to retrieve current Tenant, DB transaction error")
	}

	targetedInstanceCreation, err := common.TenantHasLegacyTargetedInstanceCreation(ctx, nil, gcth.dbSession, tn)
	if err != nil {
		logger.Error().Err(err).Msg("error resolving deprecated TargetedInstanceCreation Tenant capability")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability due to DB error", nil)
	}

	apiInstance := model.NewAPITenant(tn, targetedInstanceCreation)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Get Current Routing Profile Handler ~~~~~ //

// GetCurrentTenantRoutingProfileHandler retrieves the routing profiles the
// current Tenant may use at one Site.
type GetCurrentTenantRoutingProfileHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewGetCurrentTenantRoutingProfileHandler initializes the routing-profile handler.
func NewGetCurrentTenantRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetCurrentTenantRoutingProfileHandler {
	return GetCurrentTenantRoutingProfileHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve current Tenant routing profiles for a Site
// @Description Retrieve the Tenant's default VPC routing profile and the profiles it may select at one Site.
// @Tags tenant
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Success 200 {object} model.APITenantRoutingProfile
// @Router /v2/org/{org}/nico/tenant/current/routing-profile [get]
func (gctrph GetCurrentTenantRoutingProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TenantRoutingProfile", "GetCurrent", c, gctrph.tracerSpan)
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
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	siteID := c.QueryParam("siteId")
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, gctrph.dbSession)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in query", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in query", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in query must be in Registered state", nil)
	}
	if site.Config == nil || !site.Config.NativeNetworking {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in query must have native networking enabled", nil)
	}

	tenant, err := common.GetTenantForOrg(ctx, nil, gctrph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	allocationDAO := cdbm.NewAllocationDAO(gctrph.dbSession)
	allocationCount, err := allocationDAO.GetCount(ctx, nil, cdbm.AllocationFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		SiteIDs:   []uuid.UUID{site.ID},
	})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Allocations count from DB for Tenant and Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site Allocations count for Tenant", nil)
	}
	if allocationCount == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have any Allocations with Site specified in query", nil)
	}

	stc, err := gctrph.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	coreResponse := &corev1.FindTenantResponse{}
	apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindTenant_FullMethodName, &corev1.FindTenantRequest{
		TenantOrganizationId: org,
	}, coreResponse, site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve Tenant routing profiles")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if coreResponse.GetTenant() == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Tenant was not found on Site", nil)
	}

	allowAlternatives, err := common.TenantHasTargetedInstanceCreation(ctx, nil, gctrph.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if err != nil {
		logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify privileges for Site", nil)
	}

	response := &model.APITenantRoutingProfile{}
	response.FromProto(coreResponse, allowAlternatives)
	return c.JSON(http.StatusOK, response)
}

// ~~~~~ Get Current Stats Handler ~~~~~ //

// GetCurrentTenantStatsHandler is the API Handler for retrieving Tenant stats associated with the org
type GetCurrentTenantStatsHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetCurrentTenantStatsHandler initializes and returns a new handler to retrieve Tenant stats associate with the org
func NewGetCurrentTenantStatsHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetCurrentTenantStatsHandler {
	return GetCurrentTenantStatsHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the Tenant stats  associated with the org
// @Description Retrieve the Tenant stats associated with the org
// @Tags tenant
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APITenantStats
// @Router /v2/org/{org}/nico/tenant/current/stats [get]
func (gcth GetCurrentTenantStatsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tenant", "GetCurrentStats", c, gcth.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Tenant endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gcth.dbSession)

	tns, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}
	if len(tns) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound,
			fmt.Sprintf("Org '%v' does not have an Tenant", org), nil)
	}

	// Get VPC stats for this org tenant
	vpcDAO := cdbm.NewVpcDAO(gcth.dbSession)
	vpcStatsMap, err := vpcDAO.GetCountByStatus(ctx, nil, nil, cutil.GetPtr(tns[0].ID), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC stats for this org's tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Vpc stats", nil)
	}

	// Get Subnet stats for this org tenant
	subnetDAO := cdbm.NewSubnetDAO(gcth.dbSession)
	subnetStatsMap, err := subnetDAO.GetCountByStatus(ctx, nil, cutil.GetPtr(tns[0].ID), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Subnet stats for this org's tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnet stats", nil)
	}

	// Get Instance stats for this org tenant
	inDAO := cdbm.NewInstanceDAO(gcth.dbSession)
	instanceStats, err := inDAO.GetCountByStatus(ctx, nil, cutil.GetPtr(tns[0].ID), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Instance stats for this org's tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance stats", nil)
	}

	// Get TenantAccount stats for this org tenant
	taDAO := cdbm.NewTenantAccountDAO(gcth.dbSession)
	taStatsMap, err := taDAO.GetCountByStatus(ctx, nil, nil, cutil.GetPtr(tns[0].ID))
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving TenantAccount stats for this org's tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve TenantAccount stats", nil)
	}

	// Create response
	apiTenantStats := model.NewAPITenantStats(instanceStats, vpcStatsMap, subnetStatsMap, taStatsMap)
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiTenantStats)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateCurrentTenantHandler is the API Handler for updating the current Tenant
type UpdateCurrentTenantHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateCurrentTenantHandler initializes and returns a new handler for updating the current Tenant
func NewUpdateCurrentTenantHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateCurrentTenantHandler {
	return UpdateCurrentTenantHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update Tenant for org
// @Description Update the current Tenant for the org
// @Tags tenant
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APITenantUpdateRequest true "Tenant update request"
// @Success 200 {object} model.APITenant
// @Router /v2/org/{org}/nico/tenant/current [patch]
func (ucth UpdateCurrentTenantHandler) Handle(c echo.Context) error {
	org, dbUser, _, logger, handlerSpan := common.SetupHandler("Tenant", "UpdateCurrent", c, ucth.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Tenant endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, model.ErrMsgTenantUpdateEndpointDeprecated, nil)
}

// Utility functions
func updateTenantAccounts(ctx context.Context, dbSession *cdb.Session, tx *cdb.Tx, logger zerolog.Logger, tenant *cdbm.Tenant) error {
	// Get all TenantAccounts for this Tenant
	taDAO := cdbm.NewTenantAccountDAO(dbSession)

	tenantAccounts, _, err := taDAO.GetAll(ctx, tx, cdbm.TenantAccountFilterInput{TenantOrgs: []string{tenant.Org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving TenantAccounts for Tenant")
		return err
	}

	// Update Tenant Accounts with new Tenant ID
	for _, ta := range tenantAccounts {
		_, err := taDAO.Update(ctx, tx, cdbm.TenantAccountUpdateInput{
			TenantAccountID: ta.ID,
			TenantID:        &tenant.ID,
		})
		if err != nil {
			logger.Error().Err(err).Str("Tenant Account ID", ta.ID.String()).Msg("error updating Tenant Account with Tenant ID")
			return err
		}
	}

	return nil
}
