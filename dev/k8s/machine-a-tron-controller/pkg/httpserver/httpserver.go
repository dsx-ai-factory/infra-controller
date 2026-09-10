// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package httpserver runs the controller's small HTTP endpoints (the pod-local
// source list and the liveness endpoint) with one lifecycle: listen, serve
// until the context is cancelled, then shut down gracefully.
package httpserver

import (
	"context"
	"errors"
	"net"
	"net/http"
	"time"

	"github.com/rs/zerolog"
)

const (
	readHeaderTimeout = 5 * time.Second
	shutdownTimeout   = 5 * time.Second
)

// Run serves handler on addr until ctx is cancelled, then shuts the server
// down gracefully. It returns nil on a clean shutdown and the listen or serve
// error otherwise. name identifies the endpoint in the log line.
func Run(ctx context.Context, addr string, handler http.Handler, logger zerolog.Logger, name string) error {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}

	srv := &http.Server{
		Handler:           handler,
		ReadHeaderTimeout: readHeaderTimeout,
	}

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	logger.Info().Str("addr", ln.Addr().String()).Msg(name + " listening")

	select {
	case <-ctx.Done():
		shutdownCtx, cancel := context.WithTimeout(context.Background(), shutdownTimeout)
		defer cancel()
		if err := srv.Shutdown(shutdownCtx); err != nil {
			return err
		}
		if err := <-errCh; err != nil && !errors.Is(err, http.ErrServerClosed) {
			return err
		}
		return nil
	case err := <-errCh:
		if errors.Is(err, http.ErrServerClosed) {
			return nil
		}
		return err
	}
}
