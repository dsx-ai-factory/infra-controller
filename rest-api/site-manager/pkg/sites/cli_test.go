// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sites

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"testing"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	cli "github.com/urfave/cli/v2"
	"k8s.io/client-go/rest"
)

// TestNewCommandAcceptsSentryDSN checks that existing invocations with the
// deprecated --sentry-dsn flag still start. The flag only logs a warning.
func TestNewCommandAcceptsSentryDSN(t *testing.T) {
	// Outside a cluster the Action fails to build its Site CRD client instead
	// of serving. The warning is logged before that.
	t.Setenv("KUBERNETES_SERVICE_HOST", "")
	t.Setenv("SENTRY_DSN", "")

	var logs bytes.Buffer
	previousLogger := log.Logger
	log.Logger = zerolog.New(&logs)
	t.Cleanup(func() { log.Logger = previousLogger })

	cmd := NewCommand()
	app := &cli.App{Name: cmd.Name, Flags: cmd.Flags, Action: cmd.Action}
	err := app.RunContext(context.Background(), []string{"site-manager", "--sentry-dsn", "https://key@sentry.example/1"})
	require.ErrorIs(t, err, rest.ErrNotInCluster)

	var warnings []string
	lines := bufio.NewScanner(&logs)
	for lines.Scan() {
		entry := map[string]any{}
		require.NoError(t, json.Unmarshal(lines.Bytes(), &entry))
		if entry[zerolog.LevelFieldName] == zerolog.LevelWarnValue {
			warnings = append(warnings, entry[zerolog.MessageFieldName].(string))
		}
	}
	assert.Equal(t, []string{"--sentry-dsn / SENTRY_DSN is deprecated and ignored"}, warnings)
}
