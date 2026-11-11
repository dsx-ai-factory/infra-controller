// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"

	"github.com/labstack/echo/v4"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// HealthCheckHandler answers Kubernetes probes.
type HealthCheckHandler struct{}

// NewHealthCheckHandler returns a health check handler.
func NewHealthCheckHandler() HealthCheckHandler {
	return HealthCheckHandler{}
}

// Handle returns `200` with `ok` and a newline.
func (hch HealthCheckHandler) Handle(c echo.Context) error {
	return c.String(http.StatusOK, "ok\n")
}

// VersionHandler returns Site Manager's release version.
type VersionHandler struct {
	version string
}

// NewVersionHandler returns a handler that answers with version.
func NewVersionHandler(version string) VersionHandler {
	return VersionHandler{version: version}
}

// Handle returns `200` with the version and a newline.
func (vh VersionHandler) Handle(c echo.Context) error {
	return c.String(http.StatusOK, vh.version+"\n")
}

// MetricsHandler serves the default Prometheus registry.
type MetricsHandler struct {
	handler echo.HandlerFunc
}

// NewMetricsHandler returns a handler for the default Prometheus registry.
func NewMetricsHandler() MetricsHandler {
	return MetricsHandler{handler: echo.WrapHandler(promhttp.Handler())}
}

// Handle returns the metrics in the Prometheus text format.
func (mh MetricsHandler) Handle(c echo.Context) error {
	return mh.handler(c)
}
