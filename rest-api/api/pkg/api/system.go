// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package api

import (
	"net/http"

	apiHandler "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

const (
	healthCheckPath    = "/healthz"
	readinessCheckPath = "/readyz"
)

// NewSystemAPIRoutes returns liveness and readiness routes. Readiness uses the
// API's non-nil database session; liveness does not check dependencies.
func NewSystemAPIRoutes(dbSession *cdb.Session) []Route {
	apiRoutes := []Route{
		// Health check endpoints
		{
			Path:    healthCheckPath,
			Method:  http.MethodGet,
			Handler: apiHandler.NewHealthCheckHandler(),
		},
		{
			Path:    readinessCheckPath,
			Method:  http.MethodGet,
			Handler: apiHandler.NewReadinessCheckHandler(dbSession),
		},
	}

	return apiRoutes
}

// IsSystemRoute returns true for a path registered as SystemAPIRoute
func IsSystemRoute(p string) bool {
	return p == healthCheckPath || p == readinessCheckPath
}
