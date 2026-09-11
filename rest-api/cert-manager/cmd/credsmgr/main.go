// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package main is the command entry point
package main

import (
	"context"
	"errors"
	"os"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/certs"
	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cli "github.com/urfave/cli/v2"
)

func main() {
	cmd := certs.NewCommand()
	app := &cli.App{
		Name:    cmd.Name,
		Usage:   cmd.Usage,
		Version: "0.1.0",
		Flags:   cmd.Flags,
		Action:  cmd.Action,
	}

	ctx := core.NewDefaultContext(context.Background())
	log := core.GetLogger(ctx)
	otelShutdown, err := cotel.Bootstrap(ctx, cotel.ExporterConfigured(), "nico-rest-cert-manager")
	if err != nil {
		log.Errorf("failed to initialize tracing: %v", err)
	}

	appErr := app.RunContext(ctx, os.Args)
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	shutdownErr := otelShutdown(shutdownCtx)
	cancel()
	if shutdownErr != nil {
		log.Errorf("failed to shut down tracer provider: %v", shutdownErr)
	}
	if appErr != nil && !errors.Is(appErr, context.Canceled) {
		log.Fatal(appErr)
	}
}
