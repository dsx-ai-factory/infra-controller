// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateTenantAccountRoutingProfileHandler changes a Tenant's default at one Site.
type UpdateTenantAccountRoutingProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateTenantAccountRoutingProfileHandler initializes the routing-profile update handler.
func NewUpdateTenantAccountRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateTenantAccountRoutingProfileHandler {
	return UpdateTenantAccountRoutingProfileHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Update Tenant Account default routing profile
// @Description Change the Tenant's default VPC routing profile at a provider-owned Site.
// @Tags tenant-account
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param id path string true "ID of Tenant Account"
// @Param request body model.APITenantAccountRoutingProfileUpdateRequest true "Default routing profile update"
// @Success 200 {object} model.APITenantAccountRoutingProfile
// @Router /v2/org/{org}/nico/tenant/account/{id}/routing-profile [patch]
func (h UpdateTenantAccountRoutingProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("TenantAccountRoutingProfile", "Update", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, apiErr := common.IsProvider(ctx, logger, h.dbSession, org, dbUser, false)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	accountID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant Account ID in URL", nil)
	}
	var req model.APITenantAccountRoutingProfileUpdateRequest
	err = c.Bind(&req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid routing profile update request", err)
	}

	account, err := cdbm.NewTenantAccountDAO(h.dbSession).GetByID(ctx, nil, accountID, nil)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Tenant Account", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Tenant Account")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Account", nil)
	}
	if account.InfrastructureProviderID != provider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant Account is not associated with org's Provider", nil)
	}
	if account.Status != cdbm.TenantAccountStatusReady || account.TenantID == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant Account must be Ready and associated with a Tenant", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, req.SiteID, h.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site specified in request", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site", nil)
	}
	if site.InfrastructureProviderID != account.InfrastructureProviderID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site does not belong to Tenant Account's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered || site.Config == nil || !site.Config.NativeNetworking {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site must be Registered with native networking enabled", nil)
	}
	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	current := &corev1.FindTenantResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindTenant_FullMethodName, &corev1.FindTenantRequest{
		TenantOrganizationId: account.TenantOrg,
	}, current, site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve Tenant routing profile")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if current.GetTenant() == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Tenant was not found on Site", nil)
	}
	if current.Tenant.GetOrganizationId() != account.TenantOrg || current.Tenant.GetMetadata() == nil || current.Tenant.GetVersion() == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Site returned incomplete or mismatched Tenant data", nil)
	}

	updated := &corev1.UpdateTenantResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_UpdateTenant_FullMethodName, req.ToProto(current.Tenant), updated, site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to update Tenant routing profile")
		if apiErr.Code == http.StatusNotFound {
			apiErr.Code = http.StatusBadRequest
		}
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if updated.GetTenant().GetOrganizationId() != account.TenantOrg || updated.GetTenant().GetRoutingProfileType() != model.NormalizeAPIVpcRoutingProfileForSite(req.DefaultRoutingProfile) {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Site returned incomplete or mismatched Tenant routing profile", nil)
	}
	return c.JSON(http.StatusOK, model.APITenantAccountRoutingProfile{
		SiteID:                site.ID.String(),
		DefaultRoutingProfile: model.NormalizeAPIVpcRoutingProfileFromSite(updated.Tenant.GetRoutingProfileType()),
	})
}
