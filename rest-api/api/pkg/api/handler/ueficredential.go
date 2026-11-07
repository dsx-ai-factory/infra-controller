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
)

// CreateUEFICredentialHandler creates a site-default host or DPU UEFI credential.
type CreateUEFICredentialHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateUEFICredentialHandler returns a handler for creating a UEFI credential.
func NewCreateUEFICredentialHandler(dbSession *cdb.Session, scp *sc.ClientPool) CreateUEFICredentialHandler {
	return CreateUEFICredentialHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Create UEFI Credential
// @Description Create a site-default host or DPU UEFI credential. Equivalent to `nico-admin-cli credential add-uefi`.
// @Tags uefi-credential
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param request body model.APIUEFICredentialRequest true "UEFI credential"
// @Success 201 {object} model.APIUEFICredential
// @Router /v2/org/{org}/nico/credential/uefi [post]
func (h CreateUEFICredentialHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("UEFICredential", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIUEFICredentialRequest
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

	logger.Info().Str("kind", string(apiReq.Kind)).Str("siteID", apiReq.SiteID).Msg("creating UEFI credential via Core proxy")

	if apiErr := common.ExecuteCoreGRPC(ctx, stc, createCredentialMethod, apiReq.ToProto(), nil, siteID, "password"); apiErr != nil {
		logAPIError(logger, apiErr, "failed to create UEFI credential")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	return c.JSON(http.StatusCreated, apiReq.ToResponse())
}
