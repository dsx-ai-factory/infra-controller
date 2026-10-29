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
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateTenantRoutingProfileHandler changes a Tenant's default at one Site.
type UpdateTenantRoutingProfileHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewUpdateTenantRoutingProfileHandler initializes the routing-profile update handler.
func NewUpdateTenantRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateTenantRoutingProfileHandler {
	return UpdateTenantRoutingProfileHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Update Tenant default routing profile
// @Description Providers may update their Tenants; privileged Tenants may update their own default at a Site.
// @Tags tenant
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of caller organization"
// @Param id path string true "ID of Tenant for the provider route"
// @Param request body model.APITenantRoutingProfileUpdateRequest true "Default routing profile update"
// @Success 200 {object} model.APITenantDefaultRoutingProfile
// @Router /v2/org/{org}/nico/tenant/{id}/routing-profile [patch]
// @Router /v2/org/{org}/nico/tenant/current/routing-profile [patch]
func (h UpdateTenantRoutingProfileHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("TenantRoutingProfile", "Update", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	var provider *cdbm.InfrastructureProvider
	var tenant *cdbm.Tenant
	var apiErr *cutil.APIError
	if c.Param("id") == "" {
		tenant, apiErr = common.IsTenant(ctx, logger, h.dbSession, org, dbUser, nil)
	} else {
		provider, apiErr = common.IsProvider(ctx, logger, h.dbSession, org, dbUser, false)
	}
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	var req model.APITenantRoutingProfileUpdateRequest
	err := c.Bind(&req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid routing profile update request", err)
	}

	if provider != nil {
		tenantID, err := uuid.Parse(c.Param("id"))
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant ID in URL", nil)
		}
		tenant, err = cdbm.NewTenantDAO(h.dbSession).GetByID(ctx, nil, tenantID, nil)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Tenant", nil)
			}
			logger.Error().Err(err).Msg("failed to retrieve Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
		}
		accounts, _, err := cdbm.NewTenantAccountDAO(h.dbSession).GetAll(ctx, nil, cdbm.TenantAccountFilterInput{
			InfrastructureProviderID: &provider.ID,
			TenantIDs:                []uuid.UUID{tenant.ID},
		}, cdbp.PageInput{
			Limit: cutil.GetPtr(1),
		}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Tenant Account")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Account", nil)
		}
		if len(accounts) == 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant is not associated with org's Provider", nil)
		}
		if accounts[0].Status != cdbm.TenantAccountStatusReady {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant Account must be Ready", nil)
		}
	}

	site, err := common.GetSiteFromIDString(ctx, nil, req.SiteID, h.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site specified in request", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site", nil)
	}
	if provider != nil && site.InfrastructureProviderID != provider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site does not belong to org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered || site.Config == nil || !site.Config.NativeNetworking {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site must be Registered with native networking enabled", nil)
	}
	if provider == nil {
		privileged, err := common.TenantHasTargetedInstanceCreation(ctx, nil, h.dbSession, tenant, &common.TenantPrivilegeScope{
			SiteID: &site.ID,
		})
		if err != nil {
			logger.Error().Err(err).Msg("failed to resolve Tenant privilege for Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
		}
		if !privileged {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have Targeted Instance Creation capability enabled for Site", nil)
		}
	}
	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	current := &corev1.FindTenantResponse{}
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindTenant_FullMethodName, &corev1.FindTenantRequest{
		TenantOrganizationId: tenant.Org,
	}, current, site.ID.String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to retrieve Tenant routing profile")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if current.GetTenant() == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Tenant was not found on Site", nil)
	}
	if current.Tenant.GetOrganizationId() != tenant.Org || current.Tenant.GetMetadata() == nil || current.Tenant.GetVersion() == "" {
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
	if updated.GetTenant().GetOrganizationId() != tenant.Org || updated.GetTenant().GetRoutingProfileType() != model.NormalizeAPIVpcRoutingProfileForSite(req.DefaultRoutingProfile) {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Site returned incomplete or mismatched Tenant routing profile", nil)
	}
	return c.JSON(http.StatusOK, model.APITenantDefaultRoutingProfile{
		SiteID:                site.ID.String(),
		DefaultRoutingProfile: model.NormalizeAPIVpcRoutingProfileFromSite(updated.Tenant.GetRoutingProfileType()),
	})
}
