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

	logs := captureLogs(t)

	cmd := NewCommand()
	app := &cli.App{Name: cmd.Name, Flags: cmd.Flags, Action: cmd.Action}
	err := app.RunContext(context.Background(), []string{"site-manager", "--sentry-dsn", "https://key@sentry.example/1"})
	require.ErrorIs(t, err, rest.ErrNotInCluster)

	assert.Equal(t, []string{"--sentry-dsn / SENTRY_DSN is deprecated and ignored"}, warnings(t, logs))
}

// TestNewOptionsCertManagerFlags checks that the REST Cert Manager flags still
// accept their former creds-manager names. When both names are given, the one
// given last wins, so an override appended under the former name keeps working.
func TestNewOptionsCertManagerFlags(t *testing.T) {
	const (
		defaultURL    = "https://localhost:8000"
		defaultCAPath = "/etc/credsmgr-ca/ca.crt"
	)
	cases := []struct {
		name         string
		args         []string
		wantURL      string
		wantCAPath   string
		wantWarnings []string
	}{
		{
			name:       "defaults",
			wantURL:    defaultURL,
			wantCAPath: defaultCAPath,
		},
		{
			name:       "current names",
			args:       []string{"--cert-manager-url", "https://cm.example:8000", "--cert-manager-ca-path", "/ca/current.crt"},
			wantURL:    "https://cm.example:8000",
			wantCAPath: "/ca/current.crt",
		},
		{
			name:       "former names",
			args:       []string{"--creds-manager-url", "https://cm.example:8000", "--creds-manager-ca-path", "/ca/former.crt"},
			wantURL:    "https://cm.example:8000",
			wantCAPath: "/ca/former.crt",
			wantWarnings: []string{
				"--creds-manager-url is deprecated, use --cert-manager-url",
				"--creds-manager-ca-path is deprecated, use --cert-manager-ca-path",
			},
		},
		{
			name:         "former name given last wins",
			args:         []string{"--cert-manager-url", "https://current.example:8000", "--creds-manager-url", "https://former.example:8000"},
			wantURL:      "https://former.example:8000",
			wantCAPath:   defaultCAPath,
			wantWarnings: []string{"--creds-manager-url is deprecated, use --cert-manager-url"},
		},
		{
			name:         "current name given last wins",
			args:         []string{"--creds-manager-ca-path", "/ca/former.crt", "--cert-manager-ca-path", "/ca/current.crt"},
			wantURL:      defaultURL,
			wantCAPath:   "/ca/current.crt",
			wantWarnings: []string{"--creds-manager-ca-path is deprecated, use --cert-manager-ca-path"},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			logs := captureLogs(t)

			var got Options
			cmd := NewCommand()
			app := &cli.App{Name: cmd.Name, Flags: cmd.Flags, Action: func(c *cli.Context) error {
				got = newOptions(c)
				return nil
			}}
			require.NoError(t, app.Run(append([]string{"site-manager"}, tc.args...)))

			assert.Equal(t, tc.wantURL, got.certManagerURL)
			assert.Equal(t, tc.wantCAPath, got.certManagerCAPath)
			assert.Equal(t, tc.wantWarnings, warnings(t, logs))
		})
	}
}

// captureLogs sends the global logger to a buffer for the rest of the test.
func captureLogs(t *testing.T) *bytes.Buffer {
	t.Helper()
	var logs bytes.Buffer
	previousLogger := log.Logger
	log.Logger = zerolog.New(&logs)
	t.Cleanup(func() { log.Logger = previousLogger })
	return &logs
}

// warnings returns the messages of the warn level entries in logs.
func warnings(t *testing.T, logs *bytes.Buffer) []string {
	t.Helper()
	var messages []string
	lines := bufio.NewScanner(logs)
	for lines.Scan() {
		entry := map[string]any{}
		require.NoError(t, json.Unmarshal(lines.Bytes(), &entry))
		if entry[zerolog.LevelFieldName] == zerolog.LevelWarnValue {
			messages = append(messages, entry[zerolog.MessageFieldName].(string))
		}
	}
	return messages
}
