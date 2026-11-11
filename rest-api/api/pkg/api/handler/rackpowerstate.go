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

// GetRackPowerStateHandler returns rack power summaries, member details and outstanding work to Provider Admins.
type GetRackPowerStateHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	// boundRackID binds the literal route needed for inventory ID `task`.
	boundRackID string
}

// NewGetRackPowerStateHandler returns a handler that uses the shared site client pool for Flow requests.
func NewGetRackPowerStateHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetRackPowerStateHandler {
	return GetRackPowerStateHandler{dbSession: dbSession, scp: scp}
}

// ForRack binds an inventory ID for a literal route that would otherwise match
// the existing `/rack/task/:id` route. Ordinary rack reads use the path parameter.
func (h GetRackPowerStateHandler) ForRack(id string) GetRackPowerStateHandler {
	h.boundRackID = id
	return h
}

// Handle returns independently available rack power sections without starting power work.
// @Summary Get Rack power state
// @Tags Rack
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "External inventory Rack ID"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APIRackPowerState
// @Router /v2/org/{org}/nico/rack/{id}/power [get]
func (h GetRackPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "GetPowerState", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	infrastructureProvider, apiErr := common.IsProvider(ctx, logger, h.dbSession, org, dbUser, false)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	// Resolve ownership before obtaining the site's proxy client.
	siteStrID := c.QueryParam("siteId")
	if siteStrID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}
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

	// Echo uses RawPath when present; otherwise net/http already decoded Path.
	// Decode once so an inventory ID containing a literal percent escape survives.
	rackStrID := h.boundRackID
	if rackStrID == "" {
		rackStrID = c.Param("id")
		if c.Request().URL.RawPath != "" {
			var decodeErr error
			rackStrID, decodeErr = url.PathUnescape(rackStrID)
			if decodeErr != nil {
				logger.Warn().Err(decodeErr).Msg("invalid percent-encoding in Rack identifier")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Rack identifier in URL is invalid", nil)
			}
		}
	}
	if rackStrID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Rack identifier is required", nil)
	}
	cotel.SetAttribute(handlerSpan, attribute.String("rack_id", rackStrID))
	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Identical in-flight reads can share the site's bounded proxy request.
	request := &flowv1.GetRackPowerDetailsRequest{Id: rackStrID}
	var response flowv1.GetRackPowerDetailsResponse
	proxyErr := common.ProxyFlowGRPC(ctx, logger, stc,
		flowv1.Flow_GetRackPowerDetails_FullMethodName, request, &response,
		fmt.Sprintf("rack-power-state-%s", common.RequestHash(request)),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}
	if response.Details == nil {
		logger.Error().Msg("Flow returned no rack power state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadGateway, "Flow returned no rack power state", nil)
	}
	details := model.APIRackPowerState{}
	details.FromProto(response.Details)
	return c.JSON(http.StatusOK, details)
}
