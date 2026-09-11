// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/spf13/viper"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type logContainsWriter struct {
	needle string
	seen   chan struct{}
}

func (w logContainsWriter) Write(p []byte) (int, error) {
	if strings.Contains(string(p), w.needle) {
		select {
		case w.seen <- struct{}{}:
		default:
		}
	}
	return len(p), nil
}

func writeConfigForTest(t *testing.T, content string) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), "config.yaml")
	require.NoError(t, os.WriteFile(path, []byte(content), 0o600))
	return path
}

func TestNewConfig(t *testing.T) {
	tests := []struct {
		name string
		want *Config
	}{
		{
			name: "initialize config",
			want: &Config{},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewConfig()

			defaultPath := ProjectRoot + "/config.yaml"

			assert.Equal(t, defaultPath, got.GetPathToConfig())
		})
	}
}

func TestConfig_GetIssuersConfig(t *testing.T) {
	tests := []struct {
		name       string
		config     string
		wantErr    string
		wantLength int
		check      func(t *testing.T, issuers []IssuerConfig)
	}{
		{
			name: "claim mapping audiences",
			config: `
issuers:
  - name: custom-issuer
    issuer: https://auth.example.com
    jwks: https://auth.example.com/.well-known/jwks.json
    origin: custom
    audiences: [issuer-audience]
    claimMappings:
      - orgName: acme
        roles: [TENANT_ADMIN]
        audiences: [org-audience]
`,
			wantLength: 1,
			check: func(t *testing.T, issuers []IssuerConfig) {
				require.Len(t, issuers[0].ClaimMappings, 1)
				assert.Equal(t, []string{"issuer-audience"}, issuers[0].Audiences)
				assert.Equal(t, []string{"org-audience"}, issuers[0].ClaimMappings[0].Audiences)
			},
		},
		{
			name:   "absent issuers",
			config: "metrics:\n  enabled: true\n",
		},
		{
			name: "malformed issuer entry",
			config: `
issuers:
  - invalid
`,
			wantErr: "unmarshal issuers configuration",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			v := viper.New()
			v.SetConfigType("yaml")
			require.NoError(t, v.ReadConfig(strings.NewReader(tt.config)))

			issuers, err := (&Config{v: v}).GetIssuersConfig()
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
				return
			}

			require.NoError(t, err)
			require.Len(t, issuers, tt.wantLength)
			if tt.check != nil {
				tt.check(t, issuers)
			}
		})
	}
}

func TestConfig_ValidatePowerProvisioningConfig(t *testing.T) {
	tests := []struct {
		name      string
		values    map[string]any
		wantError string
	}{
		{
			name: "accepts disabled DPS without connection settings",
		},
		{
			name: "rejects non-boolean DPS enablement",
			values: map[string]any{
				ConfigDPSEnabled: "external",
			},
			wantError: "enabled must be a boolean",
		},
		{
			name: "requires endpoint when DPS is enabled",
			values: map[string]any{
				ConfigDPSEnabled:        true,
				ConfigDPSRequestTimeout: "15s",
			},
			wantError: "endpoint is required",
		},
		{
			name: "rejects whitespace endpoint when DPS is enabled",
			values: map[string]any{
				ConfigDPSEnabled:        true,
				ConfigDPSEndpoint:       "   ",
				ConfigDPSRequestTimeout: "15s",
			},
			wantError: "endpoint is required",
		},
		{
			name: "requires positive timeout when DPS is enabled",
			values: map[string]any{
				ConfigDPSEnabled:        true,
				ConfigDPSEndpoint:       "dps.example.com:443",
				ConfigDPSRequestTimeout: "0s",
				ConfigDPSTokenPath:      "/var/run/secrets/dps/token",
				ConfigDPSCAPath:         "/var/run/secrets/dps/ca.crt",
			},
			wantError: "must be greater than zero",
		},
		{
			name: "accepts complete enabled DPS configuration",
			values: map[string]any{
				ConfigDPSEnabled:        true,
				ConfigDPSEndpoint:       "dps.example.com:443",
				ConfigDPSRequestTimeout: "15s",
				ConfigDPSTokenPath:      "/var/run/secrets/dps/token",
				ConfigDPSCAPath:         "/var/run/secrets/dps/ca.crt",
				ConfigDPSServerName:     "dps.example.com",
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			v := viper.New()
			for key, value := range test.values {
				v.Set(key, value)
			}
			c := &Config{v: v}

			err := c.ValidatePowerProvisioningConfig()
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestConfig_ValidateIssuersConfig(t *testing.T) {
	jwtIssuer := func(name, origin string) IssuerConfig {
		return IssuerConfig{
			Name:   name,
			Origin: origin,
			Issuer: "https://" + name + ".example.com",
			JWKS:   "https://" + name + ".example.com/jwks",
		}
	}
	kasIssuer := IssuerConfig{Name: "kas-api-key", Origin: cauth.TokenOriginKas, Issuer: "https://ngc-api.example.com"}

	tests := []struct {
		name    string
		issuers []IssuerConfig
		wantErr string
	}{
		{
			name: "direct and legacy KAS",
			issuers: []IssuerConfig{
				kasIssuer,
				jwtIssuer("kas-legacy", cauth.TokenOriginKasLegacy),
			},
		},
		{
			name:    "direct KAS without rate limiter",
			issuers: []IssuerConfig{kasIssuer},
		},
		{
			name:    "direct KAS over plaintext HTTP",
			issuers: []IssuerConfig{{Name: "kas-api-key", Origin: cauth.TokenOriginKas, Issuer: "http://ngc-api.example.com"}},
			wantErr: "issuer must be an absolute HTTPS NGC API URL",
		},
		{
			name:    "direct KAS with URL credentials",
			issuers: []IssuerConfig{{Name: "kas-api-key", Origin: cauth.TokenOriginKas, Issuer: "https://user:pass@ngc-api.example.com"}},
			wantErr: "issuer must not contain user info, query, or fragment",
		},
		{
			name:    "legacy KAS alone",
			issuers: []IssuerConfig{jwtIssuer("kas-legacy", cauth.TokenOriginKasLegacy)},
		},
		{
			name: "SSA and legacy KAS",
			issuers: []IssuerConfig{
				jwtIssuer("kas-ssa", cauth.TokenOriginKasSsa),
				jwtIssuer("kas-legacy", cauth.TokenOriginKasLegacy),
			},
		},
		{
			name: "multiple custom issuers",
			issuers: []IssuerConfig{
				jwtIssuer("custom-one", cauth.TokenOriginCustom),
				jwtIssuer("custom-two", cauth.TokenOriginCustom),
			},
		},
		{
			name: "direct KAS and custom",
			issuers: []IssuerConfig{
				kasIssuer,
				jwtIssuer("custom", cauth.TokenOriginCustom),
			},
			wantErr: "origin: custom cannot be configured with any other origin",
		},
		{
			name: "legacy KAS and custom",
			issuers: []IssuerConfig{
				jwtIssuer("kas-legacy", cauth.TokenOriginKasLegacy),
				jwtIssuer("custom", cauth.TokenOriginCustom),
			},
			wantErr: "origin: custom cannot be configured with any other origin",
		},
		{
			name: "direct KAS and SSA",
			issuers: []IssuerConfig{
				kasIssuer,
				jwtIssuer("kas-ssa", cauth.TokenOriginKasSsa),
			},
			wantErr: "origin: kas and kas-ssa cannot be configured together",
		},
		{
			name:    "keycloak in the issuers list",
			issuers: []IssuerConfig{jwtIssuer("keycloak", cauth.TokenOriginKeycloak)},
			wantErr: "origin: keycloak is configured through the keycloak settings",
		},
		{
			name: "multiple direct KAS issuers",
			issuers: []IssuerConfig{
				kasIssuer,
				{Name: "kas-api-key-2", Origin: cauth.TokenOriginKas, Issuer: "https://ngc-api.example.com"},
			},
			wantErr: "only one issuer with origin: kas is allowed",
		},
		{
			name: "multiple legacy KAS issuers",
			issuers: []IssuerConfig{
				jwtIssuer("kas-legacy-one", cauth.TokenOriginKasLegacy),
				jwtIssuer("kas-legacy-two", cauth.TokenOriginKasLegacy),
			},
			wantErr: "only one issuer with origin: kas-legacy is allowed",
		},
		{
			name: "multiple SSA issuers",
			issuers: []IssuerConfig{
				jwtIssuer("kas-ssa-one", cauth.TokenOriginKasSsa),
				jwtIssuer("kas-ssa-two", cauth.TokenOriginKasSsa),
			},
			wantErr: "only one issuer with origin: kas-ssa is allowed",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			v := viper.New()
			cfg := &Config{v: v}

			err := cfg.ValidateIssuersConfig(tt.issuers)

			if tt.wantErr == "" {
				require.NoError(t, err)
			} else {
				require.ErrorContains(t, err, tt.wantErr)
			}
		})
	}
}

func TestGetOrInitTokenOriginConfig(t *testing.T) {
	t.Run("retains Keycloak config when JWKS is unavailable", func(t *testing.T) {
		testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
			res.WriteHeader(http.StatusServiceUnavailable)
		}))
		defer testServer.Close()

		v := viper.New()
		v.Set(ConfigKeycloakEnabled, true)
		v.Set(ConfigKeycloakBaseURL, testServer.URL)
		v.Set(ConfigKeycloakExternalBaseURL, "https://keycloak.example.com")
		v.Set(ConfigKeycloakRealm, "test-realm")
		v.Set(ConfigKeycloakClientID, "test-client")
		v.Set(ConfigKeycloakClientSecret, "test-secret")

		c := &Config{v: v}
		tokenOriginConfig := c.GetOrInitTokenOriginConfig()

		require.NotNil(t, tokenOriginConfig)
		jwksConfig := tokenOriginConfig.GetConfig("https://keycloak.example.com/realms/test-realm")
		require.NotNil(t, jwksConfig)
		assert.Nil(t, jwksConfig.GetJWKS())
	})
}

func TestConfig_WatchConfigFile(t *testing.T) {
	const initialSitePhoneHomeURL = "http://initial.example/phone_home"

	tests := []struct {
		name string // description of this test case
		run  func(t *testing.T, c *Config, configPath string)
	}{
		{
			name: "keeps current site phone home URL when changed config cannot be read",
			run: func(t *testing.T, c *Config, configPath string) {
				seenConfigChange := make(chan struct{}, 1)
				previousLogger := log.Logger
				log.Logger = zerolog.New(logContainsWriter{
					needle: "config file changed",
					seen:   seenConfigChange,
				})
				t.Cleanup(func() {
					log.Logger = previousLogger
				})

				require.NoError(t, os.WriteFile(configPath, []byte("site:\n  phoneHomeUrl: [\n"), 0o600))

				require.Eventually(t, func() bool {
					select {
					case <-seenConfigChange:
						return true
					default:
						return false
					}
				}, 3*time.Second, 100*time.Millisecond)
				assert.Equal(t, initialSitePhoneHomeURL, c.GetSitePhoneHomeUrl())
			},
		},
		{
			name: "reloads site phone home URL from changed config",
			run: func(t *testing.T, c *Config, configPath string) {
				const updatedSitePhoneHomeURL = "http://updated.example/phone_home"

				require.NoError(t, os.WriteFile(configPath, []byte(`
log:
  level: debug
site:
  phoneHomeUrl: http://updated.example/phone_home
`), 0o600))

				require.Eventually(t, func() bool {
					return c.GetSitePhoneHomeUrl() == updatedSitePhoneHomeURL
				}, 3*time.Second, 100*time.Millisecond)
				assert.Equal(t, "info", c.v.GetString(ConfigLogLevel))
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			configPath := writeConfigForTest(t, `
site:
  phoneHomeUrl: http://initial.example/phone_home
`)
			c := &Config{v: viper.New()}
			c.v.SetDefault(ConfigFilePath, configPath)
			c.v.SetConfigFile(configPath)
			c.v.SetDefault(ConfigLogLevel, "info")
			c.SetSitePhoneHomeUrl(initialSitePhoneHomeURL)
			c.WatchConfigFile()
			tt.run(t, c, configPath)
		})
	}
}
