// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// NICo Core (forge.Forge) credential method proxied by this handler.
const createCredentialMethod = "/forge.Forge/CreateCredential"

// CreateOrUpdateBMCCredentialHandler stores (creates or overwrites) a BMC credential.
type CreateOrUpdateBMCCredentialHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateOrUpdateBMCCredentialHandler returns a handler for creating or updating a BMC credential.
func NewCreateOrUpdateBMCCredentialHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateOrUpdateBMCCredentialHandler {
	return CreateOrUpdateBMCCredentialHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create Or Update BMC Credential
// @Description Create or update a site-wide or per-BMC root credential. Equivalent to `nico-admin-cli credential add-bmc`.
// @Tags bmc-credential
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param request body model.APIBMCCredentialRequest true "BMC credential"
// @Success 200 {object} model.APIBMCCredential
// @Router /v2/org/{org}/nico/credential/bmc [put]
func (h CreateOrUpdateBMCCredentialHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("BMCCredential", "CreateOrUpdate", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiReq model.APIBMCCredentialRequest
	if err := c.Bind(&apiReq); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid request body", nil)
	}
	if querySiteID := c.QueryParam("siteId"); querySiteID != "" {
		if apiReq.SiteID == "" {
			apiReq.SiteID = querySiteID
		} else if apiReq.SiteID != querySiteID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter does not match request body", nil)
		}
	}
	if err := apiReq.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if apiReq.Kind == model.BMCCredentialKindSiteWideRoot {
		apiReq.MacAddress = nil
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

	// Do not log the request: it contains the credential password.
	logger.Info().Str("kind", apiReq.Kind).Str("siteID", apiReq.SiteID).Msg("creating or updating BMC credential via Core proxy")

	// "password" is redacted from the Temporal payload and carried encrypted.
	if apiErr := common.ExecuteCoreGRPC(ctx, stc, createCredentialMethod, apiReq.ToProto(), nil, siteID, "password"); apiErr != nil {
		logAPIError(logger, apiErr, "failed to create or update BMC credential")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	return c.JSON(http.StatusOK, apiReq.ToResponse())
}
