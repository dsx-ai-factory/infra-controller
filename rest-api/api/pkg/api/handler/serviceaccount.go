// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/middleware"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// GetCurrentServiceAccountHandler is the API Handler for getting the current Service Account
type GetCurrentServiceAccountHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewGetCurrentServiceAccountHandler initializes and returns a new handler for getting the current Service Account
func NewGetCurrentServiceAccountHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetCurrentServiceAccountHandler {
	return GetCurrentServiceAccountHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Retrieve information about the current Service Account
// @Description Retrieve information about the current Service Account. The org's first request to any endpoint from a service account creates its Infrastructure Provider, Tenant and Tenant Account.
// @Tags serviceaccount
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of organization"
// @Success 200 {object} model.APIServiceAccount
// @Router /v2/org/{org}/nico/service-account/current [get]
func (gcsah GetCurrentServiceAccountHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ServiceAccount", "GetCurrent", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	oe := middleware.GetOrgEntity(ctx)
	if oe == nil {
		logger.Error().Msg("org entity middleware did not run for this request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current Service Account", nil)
	}

	if !oe.IsServiceAccount {
		logger.Info().Msg("service account is disabled for this org")

		return c.JSON(http.StatusOK, model.NewAPIServiceAccount(false, nil, nil))
	}

	if oe.InfrastructureProvider == nil || oe.Tenant == nil {
		logger.Error().Msg("Infrastructure Provider/Tenant for this org was not resolved by org entity middleware")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current Service Account", nil)
	}

	err := ensurePrivilegedTenantSites(ctx, gcsah.dbSession, gcsah.scp, logger, oe.Tenant, dbUser.ID)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to ensure privileged Tenant/Site associations")
	}

	// Create response
	apiServiceAccount := model.NewAPIServiceAccount(true, oe.InfrastructureProvider, oe.Tenant)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiServiceAccount)
}
