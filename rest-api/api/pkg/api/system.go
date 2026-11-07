// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package api

import (
	"net/http"

	apiHandler "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// NewSystemAPIRoutes returns liveness and readiness routes. Readiness uses the
// API's non-nil database session; liveness does not check dependencies.
func NewSystemAPIRoutes(dbSession *cdb.Session) []Route {
	apiRoutes := []Route{
		// Health check endpoints
		{
			Path:    "/healthz",
			Method:  http.MethodGet,
			Handler: apiHandler.NewHealthCheckHandler(),
		},
		{
			Path:    "/readyz",
			Method:  http.MethodGet,
			Handler: apiHandler.NewReadinessCheckHandler(dbSession),
		},
	}

	return apiRoutes
}

// IsSystemRoute returns true for a path registered as SystemAPIRoute
func IsSystemRoute(p string, dbSession *cdb.Session) bool {
	routes := NewSystemAPIRoutes(dbSession)
	for _, r := range routes {
		if r.Path == p {
			return true
		}
	}

	return false
}
