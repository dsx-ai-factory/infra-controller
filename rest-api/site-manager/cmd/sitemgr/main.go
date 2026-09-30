// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package main is the command entry point
package main

import (
	"context"
	"os"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/sitemgr"
	cli "github.com/urfave/cli/v2"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/tracing"
)

func main() {
	// First: interceptors and handlers below capture the global propagator.
	tracing.InstallPropagator()
	// No-op unless OTEL_EXPORTER_OTLP_ENDPOINT is set.
	defer tracing.InstallExporter("site-manager")()
	cmd := sitemgr.NewCommand()
	app := &cli.App{
		Name:    cmd.Name,
		Usage:   cmd.Usage,
		Version: "0.1.0",
		Flags:   cmd.Flags,
		Action:  cmd.Action,
	}

	ctx := core.NewDefaultContext(context.Background())
	log := core.GetLogger(ctx)
	if err := app.RunContext(ctx, os.Args); err != nil {
		log.Fatal(err)
	}
}
