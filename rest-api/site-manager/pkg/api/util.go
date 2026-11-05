// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package api lists Site Manager's routes.
package api

import (
	"github.com/labstack/echo/v4"
)

// RequestHandler defines the Echo compatible interface all route handlers
// implement.
type RequestHandler interface {
	Handle(c echo.Context) error
}

// Route holds the path, method and handler of one route. Name lets clients
// resolve the route's path with Echo's Reverse.
type Route struct {
	Name    string
	Path    string
	Method  string
	Handler RequestHandler
}
