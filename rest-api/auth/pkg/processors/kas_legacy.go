// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
	"context"
	"net/http"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core/claim"
	commonConfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cwfuwf "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/workflow/user"
	"github.com/golang-jwt/jwt/v5"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	temporalClient "go.temporal.io/sdk/client"
)

// MaxUserDataStalePeriod specifies the length of time between user data refresh
const MaxUserDataStalePeriod = time.Minute

// Ensure KASProcessor implements config.TokenProcessor interface
var _ config.TokenProcessor = (*KASProcessor)(nil)

// KASProcessor processes KAS JWT tokens.
//
// Deprecated: Support is limited to existing deployments.
type KASProcessor struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	encCfg    *commonConfig.PayloadEncryptionConfig
}

// ProcessToken processes KAS JWT tokens
func (h *KASProcessor) ProcessToken(c echo.Context, tokenStr string, jwksCfg *config.JwksConfig, logger zerolog.Logger) (*cdbm.User, *cutil.APIError) {
	claims := &claim.NgcKasClaims{}

	token, err := jwksCfg.ValidateToken(tokenStr, claims)
	if err != nil {
		if strings.Contains(err.Error(), jwt.ErrTokenExpired.Error()) {
			logger.Error().Err(err).Msg("Token expired")
			return nil, cutil.NewAPIError(http.StatusUnauthorized, "Authorization token in request has expired", nil)
		} else {
			logger.Error().Err(err).Msg("failed to validate JWT token in authorization header")
			return nil, cutil.NewAPIError(http.StatusUnauthorized, "Invalid authorization token in request", nil)
		}
	}

	// KAS token, extract claims from the token
	claims, ok := token.Claims.(*claim.NgcKasClaims)
	if !ok || claims == nil {
		logger.Error().Msg("claims are nil after type assertion")
		return nil, cutil.NewAPIError(http.StatusUnauthorized, "Invalid claims in authorization token", nil)
	}

	auxID, _ := token.Claims.GetSubject()
	if auxID == "" {
		return nil, cutil.NewAPIError(http.StatusUnauthorized, "Invalid authorization token, could not find subject ID in claim", nil)
	}

	// First try to find user by auxiliary ID
	userDAO := cdbm.NewUserDAO(h.dbSession)

	users, _, err := userDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
		AuxiliaryIDs: []string{auxID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(1),
	}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get user by auxiliary ID")
		return nil, cutil.NewAPIError(http.StatusUnauthorized, "Failed to retrieve user record, DB error", nil)
	}

	var dbUser *cdbm.User
	if len(users) > 0 {
		dbUser = &users[0]
	}

	if dbUser == nil || dbUser.OrgData == nil || time.Since(dbUser.Updated) > MaxUserDataStalePeriod {
		wid, err := cwfuwf.ExecuteUpdateUserFromNGCWithAuxiliaryIDWorkflow(context.Background(), h.tc, auxID, token.Raw, h.encCfg.EncryptionKey, true)

		if err != nil {
			logger.Error().Err(err).Msg("failed to execute workflow to retrieve latest user org/role info from NGC")
			return nil, cutil.NewAPIError(http.StatusUnauthorized, "Failed to retrieve latest user org/role info from NGC", nil)
		}

		logger.Info().Str("Workflow ID", *wid).Msg("executed workflow to update user data from NGC")

		// Retrieve updated user from DB after workflow
		users, _, err = userDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
			AuxiliaryIDs: []string{auxID},
		}, paginator.PageInput{
			Limit: cutil.GetPtr(1),
		}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve user from DB")
			return nil, cutil.NewAPIError(http.StatusUnauthorized, "Failed to retrieve user record, DB error", nil)
		}

		if len(users) > 0 {
			dbUser = &users[0]
		} else {
			logger.Error().Msg("user not found after workflow execution")
			return nil, cutil.NewAPIError(http.StatusUnauthorized, "Failed to retrieve user record after workflow execution", nil)
		}

	}

	// KAS tokens are user tokens, not service account tokens
	config.SetIsServiceAccountInContext(c, false)

	// Set user in context
	c.Set("user", dbUser)
	return dbUser, nil
}
