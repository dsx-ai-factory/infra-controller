// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/dsx-ai-factory/infra-controller/rest-api/api/pkg/api/model"
)

// HealthCheckHandler is an API handler to return health status of the API server
type HealthCheckHandler struct{}

// NewHealthCheckHandler creates and returns a new handler
func NewHealthCheckHandler() HealthCheckHandler {
	return HealthCheckHandler{}
}

// Handle godoc
// @Summary Returns the health status of API server
// @Description Returns the health status of the API server
// @Tags health
// @Accept */*
// @Produce json
// @Success 200 {object} model.APIHealthCheck
// @Router /healthz [get]
func (hch HealthCheckHandler) Handle(c echo.Context) error {
	ahc := model.NewAPIHealthCheck(true, nil)
	return c.JSON(http.StatusOK, ahc)
}
