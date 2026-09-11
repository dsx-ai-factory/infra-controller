// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/metadata"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	components "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/rs/zerolog/log"
)

// InitElektra initializes the Elektra site agent framework
func InitElektra() {
	// Initialize Elektra microservice
	log.Info().Msg("Elektra: Initializing Elektra service")

	// TODO: this is for verification that we can get version, will move it to a metric after
	log.Info().Msgf("Elektra: version=%s, time=%s", metadata.Version, metadata.BuildTime)

	// Initialize Elektra Data Structures
	elektraTypes := elektratypes.NewElektraTypes()

	// Initialize Elektra API
	api, initErr := components.NewElektraAPI(elektraTypes, false)
	if initErr != nil {
		log.Error().Err(initErr).Msg("Elektra: Failed to initialize Elektra API")
	} else {
		log.Info().Msg("Elektra: Successfully initialized Elektra API")
	}

	// Initialize Elektra Managers
	api.Init()

	// Start Elektra Managers
	api.Start()
}

func main() {
	// Initialize tracing before the managers so Temporal/gRPC instrumentation
	// picks up the global tracer provider; enabled purely by OTEL_* env vars
	otelShutdown, err := cotel.Bootstrap(context.Background(), cotel.ExporterConfigured(), "nico-rest-site-agent")
	if err != nil {
		log.Error().Err(err).Msg("Elektra: failed to initialize tracing")
	} else {
		defer func() {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			if err := otelShutdown(shutdownCtx); err != nil {
				log.Error().Err(err).Msg("Elektra: failed to shut down tracing")
			}
		}()
	}

	InitElektra()
	// sleep
	// Wait forever
	termChan := make(chan os.Signal, 1)
	signal.Notify(termChan, syscall.SIGINT, syscall.SIGTERM)
	select {
	case <-termChan:
		return
	}
}
