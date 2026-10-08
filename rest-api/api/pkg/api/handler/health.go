// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"net/http"
	"time"

	"github.com/jackc/pgx/v5/stdlib"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
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

// Check whether the API's PostgreSQL session is reachable and writable.
type ReadinessCheckHandler struct {
	dbSession *cdb.Session
}

// Create a readiness handler using the API's non-nil database session.
func NewReadinessCheckHandler(dbSession *cdb.Session) ReadinessCheckHandler {
	return ReadinessCheckHandler{dbSession: dbSession}
}

// Handle returns 503 with a sanitised error if the database is read-only, the
// check fails, or it exceeds 500 ms.
func (rch ReadinessCheckHandler) Handle(c echo.Context) error {
	ctx, cancel := context.WithTimeout(c.Request().Context(), readinessCheckTimeout)
	defer cancel()

	var readOnly bool
	conn, err := rch.dbSession.DB.Conn(ctx)
	if err == nil {
		defer conn.Close()
		// This read also proves connectivity without modifying application data.
		err = conn.QueryRowContext(ctx, "SELECT current_setting('transaction_read_only')::bool").Scan(&readOnly)
		if err == nil && readOnly {
			// cdb.Session wraps pgxpool with database/sql. Closing only the SQL
			// wrapper would return this read-only connection to pgxpool, where
			// it could remain attached to a standby after routing recovers.
			err = conn.Raw(func(driverConn any) error {
				return driverConn.(*stdlib.Conn).Conn().Close(ctx)
			})
		}
	}
	if err != nil || readOnly {
		log.Warn().Err(err).Bool("read_only", readOnly).Msg("database readiness check failed")
		errorMessage := "database connection is unavailable"
		return c.JSON(http.StatusServiceUnavailable, model.NewAPIHealthCheck(false, &errorMessage))
	}

	return c.JSON(http.StatusOK, model.NewAPIHealthCheck(true, nil))
}
