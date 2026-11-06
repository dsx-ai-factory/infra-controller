// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package main is the command entry point
package main

import (
	"context"
	"errors"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	cli "github.com/urfave/cli/v2"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/metadata"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/sites"
)

const (
	// ZerologMessageFieldName specifies the field name for log message
	ZerologMessageFieldName = "msg"
	// ZerologLevelFieldName specifies the field name for log level
	ZerologLevelFieldName = "type"
)

func main() {
	// Initialize logger
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnix
	zerolog.LevelFieldName = ZerologLevelFieldName
	zerolog.MessageFieldName = ZerologMessageFieldName
	zerolog.SetGlobalLevel(zerolog.InfoLevel)

	cmd := sites.NewCommand()
	app := &cli.App{
		Name:    cmd.Name,
		Usage:   cmd.Usage,
		Version: metadata.Version,
		Flags:   cmd.Flags,
		Action:  cmd.Action,
	}

	ctx := withShutdownSignals(context.Background())
	otelShutdown, err := cotel.Bootstrap(ctx, cotel.ExporterConfigured(), "nico-rest-site-manager")
	if err != nil {
		log.Error().Err(err).Msg("failed to initialize tracing")
	}

	appErr := app.RunContext(ctx, os.Args)
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	shutdownErr := otelShutdown(shutdownCtx)
	cancel()
	if shutdownErr != nil {
		log.Error().Err(shutdownErr).Msg("failed to shut down tracer provider")
	}
	if appErr != nil && !errors.Is(appErr, context.Canceled) {
		log.Fatal().Err(appErr).Msg("Site Manager stopped with an error")
	}
}

// withShutdownSignals returns a copy of parent that the first SIGINT or
// SIGTERM cancels, which starts a graceful shutdown. A second signal exits
// the process immediately with status 1.
func withShutdownSignals(parent context.Context) context.Context {
	ctx, cancel := context.WithCancel(parent)
	signals := make(chan os.Signal, 2)
	signal.Notify(signals, os.Interrupt, syscall.SIGTERM)
	go func() {
		s := <-signals
		log.Info().Str("signal", s.String()).Msg("received a shutdown signal, shutting down")
		cancel()

		s = <-signals
		log.Info().Str("signal", s.String()).Msg("received a second shutdown signal, exiting immediately")
		os.Exit(1)
	}()

	return ctx
}
