// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
	"errors"
	"net/http"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/golang-jwt/jwt/v5"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
)

var (
	// Claim name arrays for extracting user info from different token formats
	firstNameClaims = []string{"given_name", "name", "preferred_username", "firstName", "first_name"}
	lastNameClaims  = []string{"family_name", "lastName", "last_name"}
	emailClaims     = []string{"email"}
)

// Ensure CustomProcessor implements config.TokenProcessor interface
var _ config.TokenProcessor = (*CustomProcessor)(nil)

// CustomProcessor processes custom external issuer JWT tokens.
// Supports both service accounts and user tokens with claim mappings.
type CustomProcessor struct {
	dbSession *cdb.Session
}

// ProcessToken processes custom external issuer JWT tokens
// Supports:
// - Service accounts with static roles
// - User tokens with dynamic roles from claims (via rolesAttribute)
// - User tokens with static roles (via roles list)
// - Dynamic org extraction from claims (via orgAttribute)
// - Static org assignment from config (via orgName)
// - Issuer-level audience and scope validation (validated FIRST)
// - Org access validation BEFORE any DB operations
func (h *CustomProcessor) ProcessToken(c echo.Context, tokenStr string, jwksConfig *config.JwksConfig, logger zerolog.Logger) (*cdbm.User, *util.APIError) {
	// Use map claims to be able to extract custom claims like scopes
	claims := jwt.MapClaims{}

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

	// Extract claims from the token
	claims, ok := token.Claims.(jwt.MapClaims)
	if !ok || claims == nil {
		logger.Error().Msg("claims are nil after type assertion")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid claims in authorization token", nil)
	}

	// Extract necessary information from claims
	sub, _ := token.Claims.GetSubject()
	if sub == "" {
		return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid authorization token, could not find subject ID in claim", nil)
	}

	// Get org name from route (validated by middleware)
	reqOrgFromRoute := strings.ToLower(c.Param("orgName"))

	// Step 1: Validate issuer-level audiences and scopes FIRST
	if err := jwksConfig.ValidateAudience(claims); err != nil {
		logger.Warn().Err(err).Msg("Token audience does not match issuer configuration")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Token audience does not match issuer configuration", nil)
	}
	if err := jwksConfig.ValidateScopes(claims); err != nil {
		logger.Warn().Err(err).Msg("Token scopes do not match issuer requirements")
		return nil, util.NewAPIError(http.StatusForbidden, "Token scopes do not match required scopes for issuer", nil)
	}

	// Step 2: Extract org data (access already validated, this builds the full orgData)
	orgData, isServiceAccount, err := jwksConfig.GetOrgDataFromClaim(claims, reqOrgFromRoute)
	if err != nil {
		// Handle specific error types with appropriate HTTP status codes
		switch {
		case errors.Is(err, core.ErrReservedOrgName):
			logger.Warn().Err(err).Str("requested_org", reqOrgFromRoute).Msg("Organization cannot be authorized dynamically using claims data")
			return nil, util.NewAPIError(http.StatusForbidden, "Organization cannot be authorized dynamically using claims data", nil)
		case errors.Is(err, core.ErrInvalidAudience):
			logger.Warn().Err(err).Str("requested_org", reqOrgFromRoute).Msg("Token audience is not authorized for organization specified in URL")
			return nil, util.NewAPIError(http.StatusForbidden, "Token audience is not authorized for organization specified in URL", nil)
		case errors.Is(err, core.ErrInvalidConfiguration):
			logger.Warn().Err(err).Str("requested_org", reqOrgFromRoute).Msg("No authorization configuration exists for organization specified in URL")
			return nil, util.NewAPIError(http.StatusUnauthorized, "No authorization configuration exists for organization specified in URL", nil)
		case errors.Is(err, core.ErrNoClaimRoles):
			logger.Warn().Err(err).Str("requested_org", reqOrgFromRoute).Msg("Failed to extract organization roles from claims, invalid or non-existent role data")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Failed to extract organization roles from claims, invalid or non-existent role data", nil)
		default:
			logger.Warn().Err(err).Str("requested_org", reqOrgFromRoute).Msg("Failed to extract organization data from claims, invalid claim or configuration")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Failed to extract organization data from claims, invalid claim or configuration", nil)
		}
	}

	// Note: GetOrgDataFromClaim already validates:
	// - Requested org exists in orgData (returns ErrInvalidConfiguration if not)
	// - Requested org has roles (returns ErrNoClaimRoles if not)
	// So no additional checks needed here.

	// Step 3: Build auxiliary ID for DB lookup
	auxID := sub
	if prefix := jwksConfig.GetSubjectPrefix(); prefix != "" {
		auxID = prefix + ":" + sub
	}
	// Store whether this is a service account request
	config.SetIsServiceAccountInContext(c, isServiceAccount)

	// Extract user info from claims
	firstName, lastName := GetNames(claims)
	email := GetEmail(claims)

	userDAO := cdbm.NewUserDAO(h.dbSession)
	// Use the request context so auth DB work joins the request trace and obeys cancellation.
	ctx := c.Request().Context()

	dbUser, _, err := userDAO.GetOrCreate(ctx, nil, cdbm.UserGetOrCreateInput{
		AuxiliaryID: &auxID,
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to get or create user by oidc_id in DB")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Failed to retrieve or create user record, DB error", nil)
	}

	// Get updated org data - only update the requested org, preserve others
	updatedUser, apiErr := GetUserWithUpdatedOrgData(*dbUser, orgData, reqOrgFromRoute, logger)
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

// GetEmail extracts email from claims using common email claim keys.
func GetEmail(claims jwt.MapClaims) string {
	return core.GetClaimAttributeAsString(claims, emailClaims...)
}

// GetNames extracts firstName and lastName from claims, splitting firstName if lastName is empty.
func GetNames(claims jwt.MapClaims) (firstName, lastName string) {
	firstName = core.GetClaimAttributeAsString(claims, firstNameClaims...)
	lastName = core.GetClaimAttributeAsString(claims, lastNameClaims...)

	// If lastName is empty but firstName has multiple words, split it
	if lastName == "" && firstName != "" {
		words := strings.Fields(firstName)
		if len(words) > 1 {
			firstName = words[0]
			lastName = strings.Join(words[1:], " ")
		}
	}

	return firstName, lastName
}
