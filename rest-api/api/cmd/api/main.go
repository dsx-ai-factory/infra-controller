// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"errors"
	"fmt"
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
	// The deployments do not set terminationGracePeriodSeconds, so the kubelet
	// sends SIGKILL 30 seconds after SIGTERM. The two sequential shutdown
	// budgets below must fit inside that with room for dependency cleanup.
	//
	// serverShutdownTimeout bounds the drain of in-flight requests. Handler
	// waits such as the Temporal proxy timeout ladders must complete inside
	// it.
	serverShutdownTimeout = 20 * time.Second
	// tracingShutdownTimeout bounds the final trace export flush that runs
	// after the drain. A flush to a reachable collector finishes well inside
	// this; if the collector is down, waiting longer would not help.
	tracingShutdownTimeout = 5 * time.Second
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

func run(ctx context.Context) (retErr error) {
	cfg := config.NewConfig()
	defer cfg.Close()

	// Initialize tracing before DB, Temporal, and Echo so their
	// instrumentation picks up the global tracer provider
	otelShutdown, err := cotel.Bootstrap(ctx, cfg.GetTracingEnabled(), cfg.GetTracingServiceName())
	if err != nil {
		return fmt.Errorf("failed to initialize tracing: %w", err)
	}
	defer func() {
		// Use a fresh bounded context so cancellation does not skip the final
		// exporter flush after the HTTP servers and dependencies have drained.
		shutdownCtx, cancel := context.WithTimeout(context.Background(), tracingShutdownTimeout)
		defer cancel()
		if err := otelShutdown(shutdownCtx); err != nil {
			retErr = errors.Join(retErr, fmt.Errorf("failed to shut down tracing: %w", err))
		}
	}()

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
	results := make(chan error, len(servers))
	stopping := make(chan struct{})
	for _, server := range servers {
		log.Info().Str("listenAddress", server.Server.Addr).Msg("starting HTTP server")
		go func() {
			err := server.Start(server.Server.Addr)
			if errors.Is(err, http.ErrServerClosed) {
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

	shutdownCtx, cancel := context.WithTimeout(context.Background(), serverShutdownTimeout)
	defer cancel()
	shutdownResults := make(chan error, len(servers))
	for _, server := range servers {
		go func() {
			shutdownErr := server.Shutdown(shutdownCtx)
			if shutdownErr != nil {
				shutdownErr = fmt.Errorf("failed to shut down HTTP server on %s: %w", server.Server.Addr, shutdownErr)
				if closeErr := server.Close(); closeErr != nil {
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
