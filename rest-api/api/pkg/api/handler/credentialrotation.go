// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// RotateCredentialHandler stages a site-wide credential rotation.
type RotateCredentialHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewRotateCredentialHandler returns a handler that stages a credential rotation.
func NewRotateCredentialHandler(dbSession *cdb.Session, scp *sc.ClientPool) RotateCredentialHandler {
	return RotateCredentialHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Rotate Credential
// @Description Stage a site-wide credential rotation: publish a new rotate-to secret and bump the site-wide target version. Devices converge asynchronously; poll the status endpoint to observe convergence. Equivalent to `nico-admin-cli credential rotate`.
// @Tags credential-rotation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param request body model.APICredentialRotationRequest true "Credential rotation"
// @Success 200 {object} model.APICredentialRotationResult
// @Router /v2/org/{org}/nico/credential/rotation [post]
func (h RotateCredentialHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("CredentialRotation", "Rotate", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APICredentialRotationRequest
	if err := c.Bind(&apiReq); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	if err := apiReq.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	stc, siteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: h.dbSession,
		SCP:       h.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    apiReq.SiteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Do not log the request: it may contain the explicit rotate-to password.
	logger.Info().Str("credentialType", string(apiReq.CredentialType)).Str("siteID", siteID).Msg("staging credential rotation via Core proxy")

	// "password" is redacted from the Temporal payload and carried encrypted.
	coreResp := &corev1.RotateCredentialResult{}
	if apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_RotateCredential_FullMethodName, apiReq.ToProto(), coreResp, siteID, "password"); apiErr != nil {
		logAPIError(logger, apiErr, "failed to stage credential rotation")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	var resp model.APICredentialRotationResult
	resp.FromProto(coreResp)
	return c.JSON(http.StatusOK, &resp)
}

// GetCredentialRotationStatusHandler reports convergence of a site-wide rotation.
type GetCredentialRotationStatusHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetCredentialRotationStatusHandler returns a handler that reports rotation
// convergence.
func NewGetCredentialRotationStatusHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetCredentialRotationStatusHandler {
	return GetCredentialRotationStatusHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Get Credential Rotation Status
// @Description Report convergence of an in-flight or completed site-wide credential rotation. When deviceMac is set, the counts describe just that device and a per-device detail block is returned. Equivalent to `nico-admin-cli credential rotation-status`.
// @Tags credential-rotation
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site to query"
// @Param credentialType query string true "Credential family (BMC, HostUEFI, DPUUEFI, NVOS, LockdownIKM)"
// @Param deviceMac query string false "Report only this device's convergence, matched by MAC"
// @Success 200 {object} model.APICredentialRotationStatus
// @Router /v2/org/{org}/nico/credential/rotation [get]
func (h GetCredentialRotationStatusHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("CredentialRotation", "GetStatus", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	siteID := c.QueryParam("siteId")
	if _, err := uuid.Parse(siteID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required and must be a valid UUID", nil)
	}
	credentialType := model.CredentialRotationType(c.QueryParam("credentialType"))
	if err := model.ValidateCredentialRotationType(credentialType); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	stc, resolvedSiteID, apiErr := common.AuthorizeProviderSiteForCore(common.AuthorizeProviderSiteForCoreInput{
		Ctx:       ctx,
		Logger:    logger,
		DBSession: h.dbSession,
		SCP:       h.scp,
		Org:       org,
		User:      dbUser,
		SiteID:    siteID,
	})
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	coreReq := &corev1.CredentialRotationStatusRequest{CredentialType: credentialType.ToProto()}
	if deviceMac := c.QueryParam("deviceMac"); deviceMac != "" {
		coreReq.DeviceMac = &deviceMac
	}

	logger.Info().Str("credentialType", string(credentialType)).Str("siteID", resolvedSiteID).Msg("querying credential rotation status via Core proxy")

	// Status has no secret fields, so no secret key/fields are supplied.
	coreResp := &corev1.CredentialRotationStatusResult{}
	if apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_GetCredentialRotationStatus_FullMethodName, coreReq, coreResp, ""); apiErr != nil {
		logAPIError(logger, apiErr, "failed to get credential rotation status")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	var resp model.APICredentialRotationStatus
	resp.FromProto(coreResp)
	return c.JSON(http.StatusOK, &resp)
}
