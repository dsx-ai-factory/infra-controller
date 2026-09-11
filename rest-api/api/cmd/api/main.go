// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	tClient "go.temporal.io/sdk/client"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	capis "github.com/NVIDIA/infra-controller/rest-api/api/internal/server"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"

	// Imports for API doc generation
	_ "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
)

const (
	// ZerologMessageFieldName specifies the field name for log message
	ZerologMessageFieldName = "msg"
	// ZerologLevelFieldName specifies the field name for log level
	ZerologLevelFieldName = "type"

	apiListenAddress = ":8388"
	// serverShutdownTimeout bounds the drain of in-flight requests on SIGTERM
	// and, separately, the final trace export flush that follows it. Handler
	// waits such as the Temporal proxy timeout ladders must complete inside it.
	serverShutdownTimeout = 30 * time.Second
)

// @title NVIDIA NICo REST API
// @version 1.0
// @description NICo REST API allows you to manage datacenter resources from Cloud
// @termsOfService https://ngc.nvidia.com/legal/terms

// @license.name Proprietary

// @BasePath /
// @schemes http https

// @securityDefinitions.apikey ApiKeyAuth
// @in header
// @name Authorization
func main() {
	// Initialize logger
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnix
	zerolog.LevelFieldName = ZerologLevelFieldName
	zerolog.MessageFieldName = ZerologMessageFieldName

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if err := run(ctx); err != nil {
		log.Error().Err(err).Msg("API server stopped with an error")
		os.Exit(1)
	}
}

func run(ctx context.Context) error {
	cfg := config.NewConfig()
	defer cfg.Close()

	// Initialize tracing before DB, Temporal, and Echo so their
	// instrumentation picks up the global tracer provider
	otelShutdown, err := cotel.Bootstrap(ctx, cfg.GetTracingEnabled(), cfg.GetTracingServiceName())
	if err != nil {
		log.Error().Err(err).Msg("failed to initialize tracing")
	}

	return runWithTracingShutdown(func() error {
		return runAPI(ctx, cfg)
	}, otelShutdown)
}

func runWithTracingShutdown(runAPI func() error, shutdown func(context.Context) error) (retErr error) {
	defer func() {
		// Use a fresh bounded context so cancellation does not skip the final
		// exporter flush after the HTTP servers and dependencies have drained.
		shutdownCtx, cancel := context.WithTimeout(context.Background(), serverShutdownTimeout)
		defer cancel()

		shutdownErr := shutdown(shutdownCtx)
		if shutdownErr != nil {
			retErr = errors.Join(retErr, fmt.Errorf("failed to shut down tracing: %w", shutdownErr))
		}
	}()

	return runAPI()
}

func runAPI(ctx context.Context, cfg *config.Config) error {
	dbConfig := cfg.GetDBConfig()

	// Initialize DB connection
	dbSession, err := cdb.NewSession(ctx, dbConfig.Host, dbConfig.Port, dbConfig.Name, dbConfig.User, dbConfig.Password, "")
	if err != nil {
		return fmt.Errorf("failed to initialize DB session: %w", err)
	}
	defer dbSession.Close()

	// Initialize Temporal client and namespace client
	// Client objects are expensive so they are only initialized once
	tcfg, err := cfg.GetTemporalConfig()

	if err != nil {
		return fmt.Errorf("failed to get Temporal config: %w", err)
	}

	tc, tnc, err := capis.InitTemporalClients(tcfg)

	if err != nil {
		return fmt.Errorf("failed to create Temporal clients: %w", err)
	}
	defer tc.Close()
	defer tnc.Close()

	_, err = tc.CheckHealth(ctx, &tClient.CheckHealthRequest{})
	if err != nil {
		return fmt.Errorf("failed to check Temporal health: %w", err)
	}

	scp := sc.NewClientPool(tcfg)

	var powerProvisioner dpsclient.PowerProvisioner
	if cfg.GetDPSEnabled() {
		dps, err := dpsclient.NewClient(cfg.GetDPSConfig())
		if err != nil {
			return fmt.Errorf("failed to initialize DPS client: %w", err)
		}
		defer dps.Close()
		powerProvisioner = dps
	}

	// Initialize API Echo instance
	e := capis.InitAPIServer(cfg, dbSession, tc, tnc, scp, powerProvisioner)
	e.Server.Addr = apiListenAddress
	servers := []*echo.Echo{e}

	mconfig := cfg.GetMetricsConfig()
	if mconfig.Enabled {
		// Initialize Prometheus Echo instance
		ep := capis.InitMetricsServer(e, mconfig.Namespace)
		ep.Server.Addr = mconfig.GetListenAddr()
		servers = append(servers, ep)
	}

	return serveEchoServers(ctx, servers)
}

func serveEchoServers(ctx context.Context, servers []*echo.Echo) error {
	return serveEchoServersWithListenerFactory(ctx, servers, net.Listen)
}

func serveEchoServersWithListenerFactory(
	ctx context.Context,
	servers []*echo.Echo,
	listen func(network, address string) (net.Listener, error),
) error {
	if len(servers) == 0 || ctx.Err() != nil {
		return nil
	}

	listeners := make([]net.Listener, 0, len(servers))
	for _, server := range servers {
		listener, err := listen("tcp", server.Server.Addr)
		if err != nil {
			for _, opened := range listeners {
				_ = opened.Close()
			}
			return fmt.Errorf("failed to listen for HTTP server on %s: %w", server.Server.Addr, err)
		}
		listeners = append(listeners, listener)
	}
	for i, server := range servers {
		server.Listener = listeners[i]
	}
	defer func() {
		for _, listener := range listeners {
			_ = listener.Close()
		}
	}()

	results := make(chan error, len(servers))
	stopping := make(chan struct{})
	for _, server := range servers {
		log.Info().Str("listenAddress", server.Listener.Addr().String()).Msg("starting HTTP server")
		go func() {
			err := server.Start(server.Server.Addr)
			if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) {
				err = nil
			}
			select {
			case <-stopping:
			default:
				if err == nil {
					err = errors.New("server stopped unexpectedly")
				}
			}
			if err != nil {
				err = fmt.Errorf("HTTP server on %s: %w", server.Server.Addr, err)
			}
			results <- err
		}()
	}

	completed := 0
	var runErr error
	select {
	case <-ctx.Done():
	case runErr = <-results:
		completed++
	}
	close(stopping)
	// Close listeners explicitly before Shutdown. This also covers the narrow
	// startup window where a listener is bound but Serve has not entered yet.
	for _, listener := range listeners {
		_ = listener.Close()
	}

	shutdownCtx, cancel := context.WithTimeout(context.Background(), serverShutdownTimeout)
	defer cancel()
	shutdownResults := make(chan error, len(servers))
	for _, server := range servers {
		go func() {
			shutdownErr := server.Shutdown(shutdownCtx)
			if shutdownErr != nil {
				shutdownErr = fmt.Errorf("failed to shut down HTTP server on %s: %w", server.Server.Addr, shutdownErr)
				closeErr := server.Close()
				if closeErr != nil {
					shutdownErr = errors.Join(shutdownErr, fmt.Errorf("failed to close HTTP server on %s: %w", server.Server.Addr, closeErr))
				}
			}
			shutdownResults <- shutdownErr
		}()
	}
	for range servers {
		runErr = errors.Join(runErr, <-shutdownResults)
	}

	for ; completed < len(servers); completed++ {
		runErr = errors.Join(runErr, <-results)
	}
	return runErr
}
