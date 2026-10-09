// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
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

// Leave time to return the response within Kubernetes' default one-second
// probe timeout. This budget includes waiting for a database connection.
const readinessCheckTimeout = 500 * time.Millisecond

// Check whether the API's PostgreSQL session can query the site table.
type ReadinessCheckHandler struct {
	dbSession *cdb.Session
}

// Create a readiness handler using the API's non-nil database session.
func NewReadinessCheckHandler(dbSession *cdb.Session) ReadinessCheckHandler {
	return ReadinessCheckHandler{dbSession: dbSession}
}

// Handle returns 503 with a sanitised error if the site count query fails or
// exceeds 500 ms.
func (rch ReadinessCheckHandler) Handle(c echo.Context) error {
	ctx, cancel := context.WithTimeout(c.Request().Context(), readinessCheckTimeout)
	defer cancel()

	siteDAO := cdbm.NewSiteDAO(rch.dbSession)
	_, err := siteDAO.GetCount(ctx, nil, cdbm.SiteFilterInput{})
	if err != nil {
		log.Warn().Err(err).Msg("database readiness check failed")
		errorMessage := "database connection is unavailable"
		return c.JSON(http.StatusServiceUnavailable, model.NewAPIHealthCheck(false, &errorMessage))
	}

	return c.JSON(http.StatusOK, model.NewAPIHealthCheck(true, nil))
}
