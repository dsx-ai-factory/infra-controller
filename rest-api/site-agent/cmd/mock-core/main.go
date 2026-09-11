// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"flag"
	"time"

	"github.com/rs/zerolog/log"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	gsv "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/server"
)

// Test the nico grpc client
func main() {
	toutPtr := flag.Int("tout", 300, "grpc server timeout")
	flag.Parse()

	// Initialize tracing so the otelgrpc server handler emits real spans and
	// continues traces propagated by instrumented clients; enabled purely by
	// OTEL_* env vars, a no-op otherwise.
	otelShutdown, otelErr := cotel.Bootstrap(context.Background(), cotel.ExporterConfigured(), "nico-mock-core")
	if otelErr != nil {
		log.Error().Err(otelErr).Msg("failed to initialize tracing")
	} else {
		defer func() {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			if err := otelShutdown(shutdownCtx); err != nil {
				log.Error().Err(err).Msg("failed to shut down tracing")
			}
		}()
	}

	gsv.NICoTest(*toutPtr)
}
