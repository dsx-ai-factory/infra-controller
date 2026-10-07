// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// NICo Core (forge.Forge) methods proxied by this handler.
const (
	upsertHostFirmwareConfigMethod = "/forge.Forge/UpsertHostFirmwareConfig"
	deleteHostFirmwareConfigMethod = "/forge.Forge/DeleteHostFirmwareConfig"
)

// CreateOrUpdateHostFirmwareConfigHandler handles PUT /firmware-config/host.
type CreateOrUpdateHostFirmwareConfigHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateOrUpdateHostFirmwareConfigHandler returns a new CreateOrUpdateHostFirmwareConfigHandler
func NewCreateOrUpdateHostFirmwareConfigHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateOrUpdateHostFirmwareConfigHandler {
	return CreateOrUpdateHostFirmwareConfigHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Create or update a HostFirmwareConfig
// @Description Create or update a HostFirmwareConfig
// @Tags HostFirmwareConfig
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIHostFirmwareConfigCreateOrUpdateRequest true "HostFirmwareConfig create/update request"
// @Success 201 {object} model.APIHostFirmwareConfig "Config created on first call"
// @Success 200 {object} model.APIHostFirmwareConfig "Config replaced/updated"
// @Failure 503 {object} util.APIError
// @Router /v2/org/{org}/nico/firmware-config/host [put]
func (uhfch CreateOrUpdateHostFirmwareConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("HostFirmwareConfig", "CreateOrUpdate", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APIHostFirmwareConfigCreateOrUpdateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating Host Firmware Config create/update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Host Firmware Config create/update request data", verr)
	}

	temporalClient, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: uhfch.dbSession,
		SCP:       uhfch.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiRequest.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	protoRequest := apiRequest.ToProto()

	logger.Info().
		Str("vendor", apiRequest.Vendor).
		Str("model", apiRequest.Model).
		Str("siteID", apiRequest.SiteID).
		Msg("upserting Host Firmware Config via Core proxy")

	var protoResponse corev1.HostFirmwareConfigResponse
	apiErr = common.ExecuteCoreGRPC(ctx, temporalClient, upsertHostFirmwareConfigMethod, protoRequest, &protoResponse, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to upsert Host Firmware Config via Core proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	apiConfig := &model.APIHostFirmwareConfig{}
	apiConfig.FromProto(&protoResponse)
	status := http.StatusOK
	if apiConfig.IsCreated() {
		status = http.StatusCreated
	}

	return c.JSON(status, apiConfig)
}

// DeleteHostFirmwareConfigHandler handles DELETE /firmware-config/host.
type DeleteHostFirmwareConfigHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewDeleteHostFirmwareConfigHandler returns a new DeleteHostFirmwareConfigHandler.
func NewDeleteHostFirmwareConfigHandler(dbSession *cdb.Session, scp *sc.ClientPool) DeleteHostFirmwareConfigHandler {
	return DeleteHostFirmwareConfigHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Delete a HostFirmwareConfig
// @Description Delete a HostFirmwareConfig keyed by vendor and model and site.
// @Tags HostFirmwareConfig
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIHostFirmwareConfigDeleteRequest true "HostFirmwareConfig delete request"
// @Success 204
// @Failure 503 {object} util.APIError
// @Router /v2/org/{org}/nico/firmware-config/host [delete]
func (dhfch DeleteHostFirmwareConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("HostFirmwareConfig", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APIHostFirmwareConfigDeleteRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating Host Firmware Config delete request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Host Firmware Config delete request data", verr)
	}

	temporalClient, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: dhfch.dbSession,
		SCP:       dhfch.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiRequest.SiteID,
	})

	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	protoRequest := apiRequest.ToProto()

	logger.Info().
		Str("vendor", apiRequest.Vendor).
		Str("model", apiRequest.Model).
		Str("siteID", apiRequest.SiteID).
		Msg("deleting Host Firmware Config via Core proxy")

	apiErr = common.ExecuteCoreGRPC(ctx, temporalClient, deleteHostFirmwareConfigMethod, protoRequest, nil, siteID)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to delete Host Firmware Config via Core proxy")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	return c.NoContent(http.StatusNoContent)
}
