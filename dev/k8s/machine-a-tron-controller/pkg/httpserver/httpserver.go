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

// timeouts bounds every connection so that a stalled client cannot hold a
// connection-serving goroutine open.
type timeouts struct {
	// readHeader bounds reading a request's headers.
	readHeader time.Duration
	// read bounds reading a whole request. The handlers never read a body,
	// so net/http drains an unread one before it writes the response; a
	// client that declares a body and never sends it is cut off here.
	read time.Duration
	// write bounds writing a response.
	write time.Duration
	// idle bounds a keep-alive connection between requests.
	idle time.Duration
	// shutdown bounds the graceful shutdown of in-flight requests.
	shutdown time.Duration
}

var defaultTimeouts = timeouts{
	readHeader: 5 * time.Second,
	read:       5 * time.Second,
	write:      10 * time.Second,
	idle:       60 * time.Second,
	shutdown:   5 * time.Second,
}

// Run serves handler on addr until ctx is cancelled, then shuts the server
// down gracefully. It returns nil on a clean shutdown and the listen or serve
// error otherwise. name identifies the endpoint in the log line.
func Run(ctx context.Context, addr string, handler http.Handler, logger zerolog.Logger, name string) error {
	return run(ctx, addr, handler, logger, name, defaultTimeouts)
}

func run(ctx context.Context, addr string, handler http.Handler, logger zerolog.Logger, name string, t timeouts) error {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}

	srv := &http.Server{
		Handler:           handler,
		ReadHeaderTimeout: t.readHeader,
		ReadTimeout:       t.read,
		WriteTimeout:      t.write,
		IdleTimeout:       t.idle,
	}

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	logger.Info().Str("addr", ln.Addr().String()).Msg(name + " listening")

	select {
	case <-ctx.Done():
		shutdownCtx, cancel := context.WithTimeout(context.Background(), t.shutdown)
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
