// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authentication"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	cah "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/api/handler"
)

// AuthRoute represents an authentication route
type AuthRoute struct {
	Method  string
	Path    string
	Handler AuthHandler
}

// AuthHandler represents an authentication handler
type AuthHandler interface {
	Handle(c echo.Context) error
}

// NewAuthRoutes creates new authentication routes using cloud-auth services
// This function provides a complete authentication API that can be used by any service
func NewAuthRoutes(keycloakConfig *config.KeycloakConfig, dbSession *cdb.Session) []AuthRoute {
	if keycloakConfig == nil {
		log.Error().Msg("keycloak config is not initialized, cannot create authentication routes")
		return nil
	}
	if dbSession == nil {
		log.Error().Msg("database session is not initialized, cannot create authentication routes")
		return nil
	}

	// Initialize Keycloak auth service with PostgreSQL-backed single-use state redemption
	keycloakAuth := authentication.NewKeycloakAuthServiceWithStateDAO(
		keycloakConfig,
		cdbm.NewOAuthConsumedStateDAO(dbSession),
	)

	// Create handlers
	loginHandler := cah.NewLoginHandler(keycloakAuth)
	callbackHandler := cah.NewCallbackHandler(keycloakAuth)
	logoutHandler := cah.NewLogoutHandler(keycloakAuth)
	refreshHandler := cah.NewRefreshTokenHandler(keycloakAuth)

	return []AuthRoute{
		{
			Method:  "POST",
			Path:    "/login",
			Handler: loginHandler,
		},
		{
			Method:  "POST",
			Path:    "/callback",
			Handler: callbackHandler,
		},
		{
			Method:  "POST",
			Path:    "/logout",
			Handler: logoutHandler,
		},
		{
			Method:  "POST",
			Path:    "/refresh",
			Handler: refreshHandler,
		},
	}
}
