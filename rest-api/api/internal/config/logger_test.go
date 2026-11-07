// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"bytes"
	"encoding/json"
	"log/slog"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func decodeLogLine(t *testing.T, buf *bytes.Buffer) map[string]any {
	t.Helper()

	var line map[string]any
	require.NoError(t, json.Unmarshal(buf.Bytes(), &line))
	return line
}

func TestZerologSlogHandlerZerologLevel(t *testing.T) {
	handler := zerologSlogHandler{}

	tests := []struct {
		name  string
		level slog.Level
		want  zerolog.Level
	}{
		{
			name:  "debug",
			level: slog.LevelDebug,
			want:  zerolog.DebugLevel,
		},
		{
			name:  "info",
			level: slog.LevelInfo,
			want:  zerolog.InfoLevel,
		},
		{
			name:  "warn",
			level: slog.LevelWarn,
			want:  zerolog.WarnLevel,
		},
		{
			name:  "error",
			level: slog.LevelError,
			want:  zerolog.ErrorLevel,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, handler.zerologLevel(tt.level))
		})
	}
}

func TestZerologSlogHandlerHandleWritesAttrsAndGroups(t *testing.T) {
	var buf bytes.Buffer
	handler := zerologSlogHandler{
		logger: zerolog.New(&buf),
	}

	logger := slog.New(handler.WithAttrs([]slog.Attr{
		slog.String("component", "viper"),
	}).WithGroup("watch"))
	logger.Warn("config file changed", slog.String("event.file", "config.yaml"))

	line := decodeLogLine(t, &buf)
	assert.Equal(t, "warn", line["level"])
	assert.Equal(t, "config file changed", line["message"])
	assert.Equal(t, "viper", line["watch.component"])
	assert.Equal(t, "config.yaml", line["watch.event.file"])
}
