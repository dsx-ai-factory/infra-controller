// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certmanager

import (
	"github.com/rs/zerolog"
	"github.com/sirupsen/logrus"
)

// tlsLogHook forwards the shared reloader's diagnostics to the service logger.
// The reloader's private logrus logger discards its own formatted output.
type tlsLogHook struct {
	logger zerolog.Logger
}

func (h tlsLogHook) Levels() []logrus.Level {
	return []logrus.Level{logrus.InfoLevel, logrus.WarnLevel, logrus.ErrorLevel}
}

func (h tlsLogHook) Fire(entry *logrus.Entry) error {
	level := zerolog.InfoLevel
	switch entry.Level {
	case logrus.WarnLevel:
		level = zerolog.WarnLevel
	case logrus.ErrorLevel:
		level = zerolog.ErrorLevel
	}
	h.logger.WithLevel(level).Fields(map[string]any(entry.Data)).Msg(entry.Message)
	return nil
}
