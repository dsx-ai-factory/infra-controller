// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"fmt"
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	ccu "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/labstack/echo/v4"
)

// NotFoundHandler returns a middleware that returns a 404 status code for unmatched routes
func NotFoundHandler(cfg *config.Config) echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			// Skip auth processing for unmatched path
			if c.Path() == fmt.Sprintf("/%s/*", cfg.GetAPIRouteVersion()) {
				return ccu.NewAPIErrorResponse(c, http.StatusNotFound, "The requested path could not be found", nil)
			}

			return next(c)
		}
	}
}
