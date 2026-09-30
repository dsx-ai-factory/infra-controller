// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
	"context"
	"net/http"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core/claim"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/golang-jwt/jwt/v5"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
)

// NGC KAS Headers for SSA tokens
const (
	legacyStarfleetIDHeader = "X-Starfleet-Id" // Legacy Starfleet ID header
	starfleetIDHeader       = "NV-Actor-Id"    // Kas v2 Starfleet ID header
)

// Ensure SSAProcessor implements config.TokenProcessor interface
var _ config.TokenProcessor = (*SSAProcessor)(nil)

// SSAProcessor processes SSA JWT tokens.
//
// Deprecated: Support is limited to existing deployments.
type SSAProcessor struct {
	dbSession *cdb.Session
}

// HandleToken processes SSA JWT tokens
func (h *SSAProcessor) ProcessToken(c echo.Context, tokenStr string, jwksCfg *config.JwksConfig, logger zerolog.Logger) (*cdbm.User, *util.APIError) {
	// Check Starfleet ID Header
	starfleetID := c.Request().Header.Get(starfleetIDHeader)
	if starfleetID == "" {
		starfleetID = c.Request().Header.Get(legacyStarfleetIDHeader)
	}

	// SSA token, look up user by Starfleet ID
	if starfleetID == "" {
		logger.Warn().Msg("request received without Starfleet ID header, access denied")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Request is missing Starfleet ID header", nil)
	}

	claims := &claim.SsaClaims{}

	token, err := jwksCfg.ValidateToken(tokenStr, claims)
	if err != nil {
		if strings.Contains(err.Error(), jwt.ErrTokenExpired.Error()) {
			logger.Error().Err(err).Msg("Token expired")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Authorization token in request has expired", nil)
		} else {
			logger.Error().Err(err).Msg("failed to validate JWT token in authorization header")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid authorization token in request", nil)
		}
	}

	// SSA token, extract claims from the token
	claims, ok := token.Claims.(*claim.SsaClaims)
	if !ok || claims == nil {
		logger.Error().Msg("claims are nil after type assertion")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid claims in authorization token", nil)
	}

	if !claims.ValidateScope(claim.SsaScopeKas) {
		logger.Warn().Msg("request received without valid SSA scope, access denied")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Authorization token in request is missing valid SSA scope", nil)
	}

	userDAO := cdbm.NewUserDAO(h.dbSession)
	dbUser, _, err := userDAO.GetOrCreate(context.Background(), nil, cdbm.UserGetOrCreateInput{
		StarfleetID: &starfleetID,
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to get/create user for Starfleet ID in DB")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Failed to retrieve user record, DB error", nil)
	}

	// Get org name from URL path parameter
	orgName := c.Param("orgName")

	// SSA tokens are not service account tokens
	config.SetIsServiceAccountInContext(c, false)

	// Update user record if necessary
	updatedUser, apiErr := GetUpdatedUserFromHeaders(c, *dbUser, orgName, logger)
	if apiErr != nil {
		return nil, apiErr
	}

	if updatedUser != nil {
		var OrgDataParam cdbm.OrgData
		if updatedUser.OrgData != nil {
			OrgDataParam = updatedUser.OrgData
		}
		userDAO := cdbm.NewUserDAO(h.dbSession)
		dbUser, err = userDAO.Update(context.Background(), nil, cdbm.UserUpdateInput{
			UserID:    dbUser.ID,
			Email:     updatedUser.Email,
			FirstName: updatedUser.FirstName,
			LastName:  updatedUser.LastName,
			OrgData:   OrgDataParam,
		})
		if err != nil {
			logger.Error().Err(err).Msg("failed to update user in DB")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Failed to update user record, DB error", nil)
		}

		// TODO: Execute ReIndexTenant Workflow
	}

	// Set user in context
	c.Set("user", dbUser)
	return dbUser, nil
}
