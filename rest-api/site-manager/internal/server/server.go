// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package server builds Site Manager's Echo server and serves it over TLS.
package server

import (
	"context"
	"errors"
	"net"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"
	echoMiddleware "github.com/labstack/echo/v4/middleware"
	"github.com/rs/zerolog/log"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/internal/middleware"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/api"
)

const (
	// serviceName identifies the server to the Echo tracing middleware.
	serviceName = "nico-rest-site-manager"

	// readTimeout, writeTimeout and idleTimeout are the http.Server limits of
	// every connection.
	readTimeout  = 5 * time.Second
	writeTimeout = 10 * time.Second
	idleTimeout  = 120 * time.Second

	// handlerTimeout cancels the request context of a slower handler.
	handlerTimeout = 5 * time.Second

	// requestBodyLimit is the largest request body accepted, `1 MiB`.
	requestBodyLimit = "1M"

	// shutdownGracePeriod limits the drain once the ServeTLS context is
	// canceled. Connections still open after it are closed. The command waits
	// `5s` after cancellation before it returns, so the drain fits inside that
	// wait.
	shutdownGracePeriod = 3 * time.Second
)

// InitSiteManagerServer returns Site Manager's Echo instance with its
// middleware and routes. Metric names start with metricsNamespace. Requests
// to siteRoutes are logged. Requests to systemRoutes, such as probes and
// scrapes, aren't.
func InitSiteManagerServer(metricsNamespace string, systemRoutes, siteRoutes []api.Route) *echo.Echo {
	e := echo.New()
	e.HideBanner = true
	e.HidePort = true
	e.HTTPErrorHandler = cutil.DefaultHTTPErrorHandler

	e.Server.ReadTimeout = readTimeout
	e.Server.WriteTimeout = writeTimeout
	e.Server.IdleTimeout = idleTimeout

	e.Use(echoMiddleware.Recover())
	if cotel.TransportEnabled() {
		e.Use(otelecho.Middleware(serviceName))
	}
	e.Use(echoMiddleware.ContextTimeout(handlerTimeout))
	e.Use(echoMiddleware.BodyLimit(requestBodyLimit))

	// Route middleware runs only for requests that match a route.
	metrics := middleware.Metrics(metricsNamespace)
	for _, route := range systemRoutes {
		e.Add(route.Method, route.Path, route.Handler.Handle, metrics)
	}
	requestLogger := middleware.Logger()
	for _, route := range siteRoutes {
		e.Add(route.Method, route.Path, route.Handler.Handle, requestLogger, metrics).Name = route.Name
	}

	return e
}

// ServeTLS binds addr and serves e over TLS in the background until ctx is
// canceled. Then it drains for shutdownGracePeriod. It returns the bound
// listener, so callers can learn the port when addr is ":0".
func ServeTLS(ctx context.Context, e *echo.Echo, addr, certFile, keyFile string) (net.Listener, error) {
	listener, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, err
	}

	go func() {
		log.Info().Stringer("address", listener.Addr()).Msg("serving HTTPS")
		serveErr := e.Server.ServeTLS(listener, certFile, keyFile)
		if serveErr != nil && !errors.Is(serveErr, http.ErrServerClosed) {
			log.Error().Err(serveErr).Msg("server stopped with an error")
		}
	}()

	go func() {
		<-ctx.Done()

		shutdownCtx, cancel := context.WithTimeout(context.Background(), shutdownGracePeriod)
		defer cancel()

		log.Info().Stringer("address", listener.Addr()).Msg("shutting down the server")
		shutdownErr := e.Shutdown(shutdownCtx)
		if shutdownErr != nil {
			// Shutdown leaves the connections that missed the deadline open.
			log.Warn().Err(shutdownErr).Stringer("address", listener.Addr()).Msg("failed to shut down the server, closing it")
			closeErr := e.Close()
			if closeErr != nil {
				log.Warn().Err(closeErr).Stringer("address", listener.Addr()).Msg("failed to close the server")
			}
			return
		}
		log.Info().Stringer("address", listener.Addr()).Msg("shut down the server")
	}()

	return listener, nil
}
