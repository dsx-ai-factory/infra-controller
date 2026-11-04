// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"github.com/labstack/echo/v4"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// APIName stores apiName in each request context. Error responses report it
// as their source.
func APIName(apiName string) echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			c.Set(cutil.APINameContextKey, apiName)
			return next(c)
		}
	}
}
