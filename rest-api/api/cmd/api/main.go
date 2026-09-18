// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	tClient "go.temporal.io/sdk/client"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	capis "github.com/NVIDIA/infra-controller/rest-api/api/internal/server"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"

	// Imports for API doc generation
	_ "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/tracing"
)

const (
	// ZerologMessageFieldName specifies the field name for log message
	ZerologMessageFieldName = "msg"
	// ZerologLevelFieldName specifies the field name for log level
	ZerologLevelFieldName = "type"

	// serverShutdownTimeout bounds the drain of in-flight requests on SIGTERM.
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
	// First: interceptors and handlers below capture the global propagator.
	tracing.InstallPropagator()
	// No-op unless OTEL_EXPORTER_OTLP_ENDPOINT is set. Called explicitly below
	// rather than deferred: log.Fatal exits without running defers.
	shutdownTracing := tracing.InstallExporter("nico-rest-api")
	// Initialize logger
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnix
	zerolog.LevelFieldName = ZerologLevelFieldName
	zerolog.MessageFieldName = ZerologMessageFieldName

	cfg := config.NewConfig()
	defer cfg.Close()

	dbConfig := cfg.GetDBConfig()

	// Initialize DB connection
	dbSession, err := cdb.NewSession(context.Background(), dbConfig.Host, dbConfig.Port, dbConfig.Name, dbConfig.User, dbConfig.Password, "")
	if err != nil {
		log.Panic().Err(err).Msg("failed to initialize DB session")
	} else {
		defer dbSession.Close()
	}

	// Initialize Temporal client and namespace client
	// Client objects are expensive so they are only initialized once
	tcfg, err := cfg.GetTemporalConfig()

	if err != nil {
		log.Panic().Err(err).Msg("failed to get Temporal config")
	}

	tc, tnc, err := capis.InitTemporalClients(tcfg, cfg.GetTracingEnabled())

	if err != nil {
		log.Panic().Err(err).Msg("failed to create Temporal clients")
	} else {
		defer tc.Close()
		defer tnc.Close()
	}

	_, err = tc.CheckHealth(context.Background(), &tClient.CheckHealthRequest{})
	if err != nil {
		log.Panic().Err(err).Msg("failed to check Temporal health")
	}

	scp := sc.NewClientPool(tcfg)

	var powerProvisioner dpsclient.PowerProvisioner
	if cfg.GetDPSEnabled() {
		dps, err := dpsclient.NewClient(cfg.GetDPSConfig())
		if err != nil {
			log.Panic().Err(err).Msg("failed to initialize DPS client")
		}
		defer dps.Close()
		powerProvisioner = dps
	}

	// Initialize API Echo instance
	e := capis.InitAPIServer(cfg, dbSession, tc, tnc, scp, powerProvisioner)

	mconfig := cfg.GetMetricsConfig()
	if mconfig.Enabled {
		// Initialize Prometheus Echo instance
		ep := capis.InitMetricsServer(e, mconfig.Namespace)

		// Start Prometheus server
		log.Info().Msg("starting Metrics server")
		go func() {
			ep.Logger.Fatal(ep.Start(mconfig.GetListenAddr()))
		}()
	}

	// Start main server
	log.Info().Msg("starting API server")
	errCh := make(chan error, 1)
	go func() {
		errCh <- e.Start(":8388")
	}()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	defer signal.Stop(sigCh)

	select {
	case err := <-errCh:
		shutdownTracing()
		log.Fatal().Err(err).Msg("API server stopped")
	case sig := <-sigCh:
		log.Info().Stringer("signal", sig).Msg("shutting down API server")
		ctx, cancel := context.WithTimeout(context.Background(), serverShutdownTimeout)
		defer cancel()
		if err := e.Shutdown(ctx); err != nil {
			log.Error().Err(err).Msg("API server graceful shutdown failed")
		}
		// After the drain, so spans recorded by in-flight requests are exported.
		shutdownTracing()
	}
}
