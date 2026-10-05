// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package api

import (
	"net/http"

	apiHandler "github.com/dsx-ai-factory/infra-controller/rest-api/api/pkg/api/handler"
)

// NewSystemAPIRoutes returns API routes that provide system level  functions
func NewSystemAPIRoutes() []Route {
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
			Handler: apiHandler.NewHealthCheckHandler(),
		},
	}

	return apiRoutes
}

// IsSystemRoute returns true for a path registered as SystemAPIRoute
func IsSystemRoute(p string) bool {
	routes := NewSystemAPIRoutes()
	for _, r := range routes {
		if r.Path == p {
			return true
		}
	}

	return false
}
