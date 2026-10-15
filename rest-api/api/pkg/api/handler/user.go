// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"fmt"
	"net/http"

	"github.com/labstack/echo/v4"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// GetUserHandler is an API Handler to return information about the current user
type GetUserHandler struct {
	dbSession *cdb.Session
}

// NewGetUserHandler creates and returns a new handler
func NewGetUserHandler(dbSession *cdb.Session) GetUserHandler {
	return GetUserHandler{
		dbSession: dbSession,
	}
}

// Handle godoc
// @Summary Return information about the current user
// @Description Get basic information about the user making the request
// @Tags user
// @Accept */*
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APIUser
// @Router /v2/org/{org}/nico/user/current [get]
func (guh GetUserHandler) Handle(c echo.Context) error {
	org, dbUser, _, logger, handlerSpan := common.SetupHandler("User", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	apiUser := model.NewAPIUserFromDBUser(*dbUser)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiUser)
}
