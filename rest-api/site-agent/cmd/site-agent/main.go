// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"os"
	"os/signal"
	"syscall"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/metadata"

	components "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/tracing"
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
	// First: interceptors and handlers below capture the global propagator.
	tracing.InstallPropagator()
	// No-op unless OTEL_EXPORTER_OTLP_ENDPOINT is set.
	defer tracing.InstallExporter("site-agent")()
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
