// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"github.com/labstack/echo/v4"
)

// APIName returns a middleware that sets the API name in the request context
func APIName(apiName string) echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			c.Set("apiName", apiName)
			return next(c)
		}
	}
}
