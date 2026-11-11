// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"fmt"
	"net/http"
	"net/url"

	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
	temporalEnums "go.temporal.io/api/enums/v1"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// GetTrayPowerStateHandler returns tray power state and operations to Provider Admins.
type GetTrayPowerStateHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetTrayPowerStateHandler uses the shared site client pool for Flow requests.
func NewGetTrayPowerStateHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetTrayPowerStateHandler {
	return GetTrayPowerStateHandler{dbSession: dbSession, scp: scp}
}

// Handle returns independently available tray power sections without starting power work.
// @Summary Get Tray power state
// @Tags tray
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "Component ID or component MAC address"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APITrayPowerState
// @Router /v2/org/{org}/nico/tray/{id}/power [get]
func (h GetTrayPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "GetPowerState", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	infrastructureProvider, apiErr := common.IsProvider(ctx, logger, h.dbSession, org, dbUser, false)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	// Validate siteId is provided
	siteStrID := c.QueryParam("siteId")
	if siteStrID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, siteStrID, h.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid siteId", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	if !siteConfig.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	// Echo routes RawPath when present; otherwise net/http has already
	// decoded Path, including a literal percent sign in the identifier.
	trayStrID := c.Param("id")
	if c.Request().URL.RawPath != "" {
		var decodeErr error
		trayStrID, decodeErr = url.PathUnescape(trayStrID)
		if decodeErr != nil {
			logger.Warn().Err(decodeErr).Msg("invalid percent-encoding in Tray identifier")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tray identifier in URL is invalid", nil)
		}
	}
	cotel.SetAttribute(handlerSpan, attribute.String("tray_id", trayStrID))

	// Get the temporal client for the site
	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Identical in-flight reads can share the site's bounded proxy request.
	request := &flowv1.GetTrayPowerDetailsRequest{Id: trayStrID}
	var response flowv1.GetTrayPowerDetailsResponse
	proxyErr := common.ProxyFlowGRPC(ctx, logger, stc,
		flowv1.Flow_GetTrayPowerDetails_FullMethodName, request, &response,
		fmt.Sprintf("tray-power-state-%s", common.RequestHash(request)),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}
	if response.Details == nil {
		logger.Error().Msg("Flow returned no tray power state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadGateway, "Flow returned no tray power state", nil)
	}

	details := model.APITrayPowerState{}
	details.FromProto(response.Details)
	return c.JSON(http.StatusOK, details)
}
