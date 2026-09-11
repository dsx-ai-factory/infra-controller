// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
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

// Ensure KeycloakProcessor implements config.TokenProcessor interface
var _ config.TokenProcessor = (*KeycloakProcessor)(nil)

// KeycloakProcessor processes Keycloak JWT tokens
type KeycloakProcessor struct {
	dbSession      *cdb.Session
	keycloakConfig *config.KeycloakConfig
}

// HandleToken processes Keycloak JWT tokens
func (h *KeycloakProcessor) ProcessToken(c echo.Context, tokenStr string, jwksConfig *config.JwksConfig, logger zerolog.Logger) (*cdbm.User, *util.APIError) {
	claims := &claim.KeycloakClaims{}

	token, err := jwksConfig.ValidateToken(tokenStr, claims)
	if err != nil {
		if strings.Contains(err.Error(), jwt.ErrTokenExpired.Error()) {
			logger.Error().Err(err).Msg("Token expired")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Authorization token in request has expired", nil)
		} else {
			logger.Error().Err(err).Msg("failed to validate JWT token in authorization header")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid authorization token in request", nil)
		}
	}

	// Keycloak token, extract claims from the token
	claims, ok := token.Claims.(*claim.KeycloakClaims)
	if !ok || claims == nil {
		logger.Error().Msg("claims are nil after type assertion")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid claims in authorization token", nil)
	}

	// Extract necessary information from claims
	sub, _ := token.Claims.GetSubject()
	if sub == "" {
		return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid authorization token, could not find subject ID in claim", nil)
	}

	email := claims.GetEmail()
	firstName := claims.FirstName
	lastName := claims.LastName
	auxId := claims.GetOidcId()
	if claims.GetClientId() != "" {
		// indicates service account, check if service accounts are enabled
		if !jwksConfig.ServiceAccount {
			logger.Error().Str("clientID", claims.GetClientId()).Msg("Service account detected but service accounts are not enabled")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Service accounts are not enabled", nil)
		}
		// use sub as auxId
		auxId = sub
		firstName = claims.GetClientId()
	}

	tokenOrgData := claims.ToOrgData()

	if len(tokenOrgData) == 0 {
		return nil, util.NewAPIError(http.StatusForbidden, "User does not have any roles assigned", nil)
	}

	// Get org name from URL path parameter
	reqOrgName := c.Param("orgName")

	// Set isServiceAccount in context based on clientId
	isServiceAccount := claims.GetClientId() != "" && jwksConfig.ServiceAccount
	config.SetIsServiceAccountInContext(c, isServiceAccount)

	// Use the request context so auth DB work joins the request trace and obeys cancellation.
	ctx := c.Request().Context()

	userDAO := cdbm.NewUserDAO(h.dbSession)
	dbUser, _, err := userDAO.GetOrCreate(ctx, nil, cdbm.UserGetOrCreateInput{
		AuxiliaryID: &auxId,
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to get or create user by oidc_id in DB")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Failed to retrieve or create user record, DB error", nil)
	}

	// Use GetUserWIthUpdatedOrgData to check if update is needed for the requested org
	updatedUser, apiErr := GetUserWithUpdatedOrgData(*dbUser, tokenOrgData, reqOrgName, logger)
	if apiErr != nil {
		return nil, apiErr
	}

	if updatedUser != nil {
		dbUser, err = userDAO.Update(ctx, nil, cdbm.UserUpdateInput{
			UserID:    dbUser.ID,
			Email:     &email,
			FirstName: &firstName,
			LastName:  &lastName,
			OrgData:   updatedUser.OrgData,
		})
		if err != nil {
			logger.Error().Err(err).Msg("failed to update user in DB")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Failed to update user record, DB error", nil)
		}
	}

	// Set user in context
	c.Set("user", dbUser)
	return dbUser, nil
}
