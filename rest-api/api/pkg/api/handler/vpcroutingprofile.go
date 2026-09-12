// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"net/http"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	tclient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Routing operations share provider authorization; ordinary VPC PATCH remains
// a separate tenant operation.
type vpcRoutingProfileHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

func newVPCRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) vpcRoutingProfileHandler {
	return vpcRoutingProfileHandler{dbSession: dbSession, scp: scp, tracerSpan: cutil.NewTracerSpan()}
}

// GetVPCRoutingProfileHandler reads authoritative VPC routing and allocation state.
type GetVPCRoutingProfileHandler struct {
	vpcRoutingProfileHandler
}

// NewGetVPCRoutingProfileHandler returns a provider-only routing inspection handler.
func NewGetVPCRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetVPCRoutingProfileHandler {
	return GetVPCRoutingProfileHandler{newVPCRoutingProfileHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Get VPC Routing Profile
// @Description Inspect persisted routing and allocations; this does not verify dataplane convergence.
// @Tags vpc
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param vpcId path string true "ID of VPC"
// @Success 200 {object} model.APIVpcRoutingState
// @Router /v2/org/{org}/nico/vpc/{vpcId}/routing-profile [get]
func (h GetVPCRoutingProfileHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("VPCRoutingProfile", "Get", c, h.tracerSpan)
	if span != nil {
		defer span.End()
	}

	vpc, stc, apiErr := h.authorize(ctx, logger, org, user, c.Param("id"))
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	state, apiErr := getVPCRoutingState(ctx, stc, vpc.GetSiteID().String())
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to inspect VPC routing state")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	state.VpcID = vpc.ID.String()
	return c.JSON(http.StatusOK, state)
}

// UpdateVPCRoutingProfileHandler changes a profile with one internally observed version.
type UpdateVPCRoutingProfileHandler struct {
	vpcRoutingProfileHandler
}

// NewUpdateVPCRoutingProfileHandler returns a provider-only routing update handler.
func NewUpdateVPCRoutingProfileHandler(dbSession *cdb.Session, scp *sc.ClientPool) UpdateVPCRoutingProfileHandler {
	return UpdateVPCRoutingProfileHandler{newVPCRoutingProfileHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Update VPC Routing Profile
// @Description Change the profile and active VNI, retaining the previous allocation until explicit release.
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param vpcId path string true "ID of VPC"
// @Param request body model.APIVpcRoutingProfileUpdateRequest true "Destination profile and optional VNI"
// @Success 200 {object} model.APIVpcRoutingState
// @Router /v2/org/{org}/nico/vpc/{vpcId}/routing-profile [patch]
func (h UpdateVPCRoutingProfileHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("VPCRoutingProfile", "Update", c, h.tracerSpan)
	if span != nil {
		defer span.End()
	}

	// Both Core calls share the 50-second caller budget, below the HTTP
	// server's 60-second write deadline. The second call cannot restart it.
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	vpc, stc, apiErr := h.authorize(ctx, logger, org, user, c.Param("id"))
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	var req model.APIVpcRoutingProfileUpdateRequest
	err := c.Bind(&req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	req.VpcID = vpc.GetSiteID().String()

	observed, apiErr := getVPCRoutingState(ctx, stc, req.VpcID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to inspect VPC before routing profile update")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	req.IfVersionMatch = observed.Version
	if ctx.Err() != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusGatewayTimeout, "Request expired before VPC routing profile update was submitted", nil)
	}

	// Keep this version for the only mutation attempt. A new HTTP request is
	// a new action; retrying here could change a VPC after another operator did.
	var raw corev1.VpcRoutingState
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ChangeVpcRoutingProfile_FullMethodName, req.ToProto(), &raw, "")
	if apiErr != nil {
		return vpcRoutingMutationError(c, logger, apiErr)
	}
	err = req.ValidateResponse(&raw, observed)
	if err != nil {
		return vpcRoutingMutationError(c, logger, cutil.NewAPIError(http.StatusInternalServerError, "Invalid VPC routing profile update response", err))
	}
	var state model.APIVpcRoutingState
	state.FromProto(&raw)
	logger.Info().Str("vpc_id", vpc.ID.String()).Str("site_id", vpc.SiteID.String()).Uint32("active_vni", state.ActiveVni).Msg("VPC routing profile update committed")
	state.VpcID = vpc.ID.String()
	return c.JSON(http.StatusOK, state)
}

// ReleaseVPCInactiveVniHandler releases an allocation after operator convergence checks.
type ReleaseVPCInactiveVniHandler struct {
	vpcRoutingProfileHandler
}

// NewReleaseVPCInactiveVniHandler returns a provider-only inactive VNI release handler.
func NewReleaseVPCInactiveVniHandler(dbSession *cdb.Session, scp *sc.ClientPool) ReleaseVPCInactiveVniHandler {
	return ReleaseVPCInactiveVniHandler{newVPCRoutingProfileHandler(dbSession, scp)}
}

// Handle godoc
// @Summary Release VPC Inactive VNI
// @Description Release the exact inactive allocation after independently verifying all affected consumers.
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of provider organization"
// @Param vpcId path string true "ID of VPC"
// @Param request body model.APIVpcInactiveVniReleaseRequest true "Originally observed version and inactive VNI"
// @Success 200 {object} model.APIVpcInactiveVniReleaseResult
// @Router /v2/org/{org}/nico/vpc/{vpcId}/routing-profile/release-inactive-vni [post]
func (h ReleaseVPCInactiveVniHandler) Handle(c echo.Context) error {
	org, user, ctx, logger, span := common.SetupHandler("VPCRoutingProfile", "ReleaseInactiveVni", c, h.tracerSpan)
	if span != nil {
		defer span.End()
	}

	vpc, stc, apiErr := h.authorize(ctx, logger, org, user, c.Param("id"))
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	var req model.APIVpcInactiveVniReleaseRequest
	err := c.Bind(&req)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	err = req.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	req.VpcID = vpc.GetSiteID().String()

	// The supplied version identifies the allocation the operator checked.
	// Do not replace it with a fresh observation or retry an uncertain release.
	var raw corev1.VpcReleaseInactiveVniResult
	apiErr = common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ReleaseVpcInactiveVni_FullMethodName, req.ToProto(), &raw, "")
	if apiErr != nil {
		return vpcRoutingMutationError(c, logger, apiErr)
	}
	err = req.ValidateResponse(&raw)
	if err != nil {
		return vpcRoutingMutationError(c, logger, cutil.NewAPIError(http.StatusInternalServerError, "Invalid inactive VNI release response", err))
	}
	var result model.APIVpcInactiveVniReleaseResult
	result.FromProto(&raw)
	logger.Info().Str("vpc_id", vpc.ID.String()).Str("site_id", vpc.SiteID.String()).Uint32("released_inactive_vni", result.ReleasedInactiveVni).Msg("VPC inactive VNI released")
	result.VpcID = vpc.ID.String()
	return c.JSON(http.StatusOK, result)
}

func (h vpcRoutingProfileHandler) authorize(ctx context.Context, logger zerolog.Logger, org string, user *cdbm.User, vpcID string) (*cdbm.Vpc, tclient.Client, *cutil.APIError) {
	if user == nil {
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	provider, apiErr := common.IsProvider(ctx, logger, h.dbSession, org, user, false)
	if apiErr != nil {
		return nil, nil, apiErr
	}
	vpc, err := common.GetVpcFromIDString(ctx, nil, vpcID, []string{cdbm.SiteRelationName}, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Invalid VPC ID in request", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return nil, nil, cutil.NewAPIError(http.StatusNotFound, "Could not find VPC with specified ID", nil)
		default:
			logger.Error().Err(err).Msg("failed to retrieve VPC for routing operation")
			return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve VPC", nil)
		}
	}
	if vpc.InfrastructureProviderID != provider.ID {
		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "VPC does not belong to current Provider", nil)
	}
	if vpc.Site == nil {
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site for VPC", nil)
	}
	if vpc.Site.InfrastructureProviderID != provider.ID {
		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "VPC Site does not belong to current Provider", nil)
	}
	if vpc.Site.Status != cdbm.SiteStatusRegistered {
		return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "VPC Site is not in Registered state", nil)
	}
	stc, err := h.scp.GetClientByID(vpc.SiteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for VPC Site")
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	return vpc, stc, nil
}

func getVPCRoutingState(ctx context.Context, stc tclient.Client, controllerID string) (*model.APIVpcRoutingState, *cutil.APIError) {
	var raw corev1.VpcRoutingState
	req := &corev1.VpcRoutingStateRequest{Id: &corev1.VpcId{Value: controllerID}}
	apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_GetVpcRoutingState_FullMethodName, req, &raw, "")
	if apiErr != nil {
		return nil, apiErr
	}
	var state model.APIVpcRoutingState
	state.FromProto(&raw)
	err := state.ValidateResponse(controllerID)
	if err != nil {
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Invalid VPC routing state response", err)
	}
	return &state, nil
}

func vpcRoutingMutationError(c echo.Context, logger zerolog.Logger, apiErr *cutil.APIError) error {
	logAPIError(logger, apiErr, "VPC routing mutation failed")
	message := apiErr.Message
	if apiErr.Code >= http.StatusInternalServerError && apiErr.Code != http.StatusNotImplemented {
		message += "; the operation may have committed, inspect VPC routing state before submitting another request"
	}
	return cutil.NewAPIErrorResponse(c, apiErr.Code, message, nil)
}
