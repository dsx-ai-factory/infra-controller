// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"net/url"
	"os"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/jackc/pgx/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestConfig_BuildDSN(t *testing.T) {
	// Keep local PostgreSQL environment settings out of the parser checks.
	for _, entry := range os.Environ() {
		name, _, _ := strings.Cut(entry, "=")
		if strings.HasPrefix(name, "PG") {
			t.Setenv(name, "")
		}
	}

	tests := []struct {
		name              string
		host              string
		wantHost          string
		caCertificatePath string
	}{
		{name: "hostname", host: "db.example.internal", wantHost: "db.example.internal"},
		{name: "IPv4", host: "192.0.2.1", wantHost: "192.0.2.1"},
		{name: "IPv6", host: "2001:db8::1", wantHost: "2001:db8::1"},
		{name: "bracketed IPv6", host: "[2001:db8::1]", wantHost: "2001:db8::1"},
		{name: "CA certificate", host: "db.example.internal", caCertificatePath: "/var/secrets/db/ca.crt"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := Config{
				Host:              tt.host,
				Port:              6432,
				DBName:            "forge",
				Credential:        credential.New("forge+%/?", "s3cr3t:@+%/?"),
				CACertificatePath: tt.caCertificatePath,
			}

			dsn := cfg.BuildDSN()
			parsedURL, err := url.Parse(dsn)
			require.NoError(t, err)

			wantQuery := url.Values{"sslmode": {"prefer"}}
			if tt.caCertificatePath != "" {
				wantQuery.Set("sslrootcert", tt.caCertificatePath)
			}
			assert.Equal(t, wantQuery, parsedURL.Query())

			if tt.caCertificatePath != "" {
				// pgx opens the CA file while parsing; this row only checks
				// that the configured path is included in the URL.
				return
			}

			parsed, err := pgx.ParseConfig(dsn)
			require.NoError(t, err)
			assert.Equal(t, tt.wantHost, parsed.Host)
			assert.EqualValues(t, cfg.Port, parsed.Port)
			assert.Equal(t, cfg.Credential.User, parsed.User)
			assert.Equal(t, cfg.Credential.Password.Value, parsed.Password)
			assert.Equal(t, cfg.DBName, parsed.Database)
		})
	}
}
