// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"log/slog"
	"slices"
	"strings"

	"github.com/rs/zerolog"
)

type zerologSlogHandler struct {
	logger zerolog.Logger
	attrs  []slog.Attr
	groups []string
}

func (h zerologSlogHandler) Enabled(context.Context, slog.Level) bool {
	return true
}

func (h zerologSlogHandler) Handle(_ context.Context, record slog.Record) error {
	event := h.logger.WithLevel(h.zerologLevel(record.Level))
	if event == nil {
		return nil
	}
	for _, attr := range h.attrs {
		h.addAttr(event, attr)
	}
	record.Attrs(func(attr slog.Attr) bool {
		h.addAttr(event, attr)
		return true
	})
	event.Msg(record.Message)
	return nil
}

func (h zerologSlogHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	h.attrs = append(slices.Clone(h.attrs), attrs...)
	return h
}

func (h zerologSlogHandler) WithGroup(name string) slog.Handler {
	if name != "" {
		h.groups = append(slices.Clone(h.groups), name)
	}
	return h
}

func (h zerologSlogHandler) addAttr(event *zerolog.Event, attr slog.Attr) {
	attr.Value = attr.Value.Resolve()
	if attr.Equal(slog.Attr{}) {
		return
	}
	key := attr.Key
	if len(h.groups) > 0 {
		key = strings.Join(append(slices.Clone(h.groups), key), ".")
	}
	event.Any(key, attr.Value.Any())
}

func (h zerologSlogHandler) zerologLevel(level slog.Level) zerolog.Level {
	switch {
	case level < slog.LevelInfo:
		return zerolog.DebugLevel
	case level < slog.LevelWarn:
		return zerolog.InfoLevel
	case level < slog.LevelError:
		return zerolog.WarnLevel
	default:
		return zerolog.ErrorLevel
	}
}
