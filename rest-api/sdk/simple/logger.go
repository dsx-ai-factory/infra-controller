// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"

	"github.com/rs/zerolog"
)

type loggerKey struct{}

// Logger is an alias for *zerolog.Logger
type Logger = *zerolog.Logger

// NewNoOpLogger returns a no-op logger that discards all log messages
func NewNoOpLogger() Logger {
	logger := zerolog.Nop()
	return &logger
}

// WithLogger returns a new context with the given logger embedded
func WithLogger(ctx context.Context, logger Logger) context.Context {
	return context.WithValue(ctx, loggerKey{}, logger)
}

// LoggerFromContext extracts the logger from the context.
// If no logger is found in the context, it returns a no-op logger.
func LoggerFromContext(ctx context.Context) Logger {
	if logger, ok := ctx.Value(loggerKey{}).(Logger); ok && logger != nil {
		return logger
	}
	return NewNoOpLogger()
}
