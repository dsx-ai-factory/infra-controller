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

func TestConfig_GetAPIPort(t *testing.T) {
	tests := []struct {
		name      string
		configure func(c *Config)
		want      int
	}{
		{
			name:      "unset falls back to the default",
			configure: func(c *Config) {},
			want:      8388,
		},
		{
			name:      "override is honored",
			configure: func(c *Config) { c.v.Set(ConfigAPIPort, 18388) },
			want:      18388,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := NewConfig()
			tt.configure(c)
			defer c.v.Set(ConfigAPIPort, 8388)

			assert.Equal(t, tt.want, c.GetAPIPort())
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
  - issuer: https://auth.example.com
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
			Origin: origin,
			Issuer: "https://" + name + ".example.com",
			JWKS:   "https://" + name + ".example.com/jwks",
		}
	}
	kasIssuer := IssuerConfig{Origin: cauth.TokenOriginKas, Issuer: "https://ngc-api.example.com"}

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
			issuers: []IssuerConfig{{Origin: cauth.TokenOriginKas, Issuer: "http://ngc-api.example.com"}},
			wantErr: "issuer must be an absolute HTTPS NGC API URL",
		},
		{
			name:    "direct KAS with URL credentials",
			issuers: []IssuerConfig{{Origin: cauth.TokenOriginKas, Issuer: "https://user:pass@ngc-api.example.com"}},
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
				{Origin: cauth.TokenOriginKas, Issuer: "https://ngc-api.example.com"},
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

// TestValidateIssuersConfigIdentityUniqueness covers the uniqueness rule shared by
// the ConfigMap, the create path, reload, and the on-demand resolver: no two
// issuers may claim the same issuer URL or JWKS URL. Each is reported as an
// identity conflict so callers can answer 409 instead of 400.
func TestValidateIssuersConfigIdentityUniqueness(t *testing.T) {
	issuer := func(issuerURL, jwksURL, orgName string) IssuerConfig {
		return IssuerConfig{
			Origin:        "custom",
			Issuer:        issuerURL,
			JWKS:          jwksURL,
			ClaimMappings: []cauth.ClaimMapping{{OrgName: orgName, Roles: []string{"TENANT_ADMIN"}}},
		}
	}
	first := issuer("https://first.example.com", "https://first.example.com/jwks", "first-org")

	tests := []struct {
		name        string
		second      IssuerConfig
		wantErrPart string
	}{
		{
			name:   "distinct_identities",
			second: issuer("https://second.example.com", "https://second.example.com/jwks", "second-org"),
		},
		{
			name:        "duplicate_issuer_url",
			second:      issuer("https://first.example.com", "https://second.example.com/jwks", "second-org"),
			wantErrPart: "duplicate issuer URL",
		},
		{
			name:        "duplicate_jwks_url",
			second:      issuer("https://second.example.com", "https://first.example.com/jwks", "second-org"),
			wantErrPart: "duplicate JWKS URL",
		},
	}

	c := &Config{v: viper.New()}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := c.ValidateIssuersConfig([]IssuerConfig{first, tt.second})

			if tt.wantErrPart == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErrPart)
			assert.ErrorIs(t, err, ErrIssuerIdentityConflict, "callers map this to 409, not 400")
		})
	}
}

// TestValidateIssuersConfigOrgOwnership covers who may own a static org name: one
// issuer. Inside that issuer the org may repeat only with non-empty, disjoint
// audiences, and it has at most one service-account mapping.
func TestValidateIssuersConfigOrgOwnership(t *testing.T) {
	issuer := func(host string, mappings ...cauth.ClaimMapping) IssuerConfig {
		return IssuerConfig{
			Origin:        "custom",
			Issuer:        "https://" + host + ".example.com",
			JWKS:          "https://" + host + ".example.com/jwks",
			ClaimMappings: mappings,
		}
	}
	tenantAdmin := cauth.ClaimMapping{OrgName: "Acme", Roles: []string{"TENANT_ADMIN"}}
	providerAdmin := cauth.ClaimMapping{OrgName: "acme", Roles: []string{"PROVIDER_ADMIN"}}
	audA := cauth.ClaimMapping{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}, Audiences: []string{"a"}}
	audB := cauth.ClaimMapping{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}, Audiences: []string{"b"}}
	serviceAccountA := cauth.ClaimMapping{OrgName: "acme", IsServiceAccount: true, Audiences: []string{"sa-a"}}
	serviceAccountB := cauth.ClaimMapping{OrgName: "acme", IsServiceAccount: true, Audiences: []string{"sa-b"}}

	tests := []struct {
		name        string
		issuers     []IssuerConfig
		wantErrPart string
	}{
		{
			name:    "distinct_orgs",
			issuers: []IssuerConfig{issuer("first", tenantAdmin), issuer("second", cauth.ClaimMapping{OrgName: "beta", Roles: []string{"TENANT_ADMIN"}})},
		},
		{
			name:        "org_owned_by_two_issuers",
			issuers:     []IssuerConfig{issuer("first", tenantAdmin), issuer("second", providerAdmin)},
			wantErrPart: "an org may belong to exactly one issuer",
		},
		{
			name:        "repeat_without_audiences",
			issuers:     []IssuerConfig{issuer("first", tenantAdmin, providerAdmin)},
			wantErrPart: "overlapping or empty audiences",
		},
		{
			name:    "repeat_with_disjoint_audiences",
			issuers: []IssuerConfig{issuer("first", audA, audB)},
		},
		{
			name:        "second_service_account",
			issuers:     []IssuerConfig{issuer("first", serviceAccountA, serviceAccountB)},
			wantErrPart: "already has a service account mapping",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := &Config{v: viper.New()}
			c.v.Set(ConfigEnvDisconnected, true)

			err := c.ValidateIssuersConfig(tt.issuers)

			if tt.wantErrPart == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErrPart)
		})
	}
}

// TestValidateIssuersConfigJWKSTimeout covers the bound on how long one issuer's
// IdP may hold a token-path request. Parsing alone was not enough: a zero or
// negative duration silently reverted to the default, and an arbitrarily large
// one made the timeout meaningless.
// TestValidateIssuersConfigRejectsBlankListElements: an empty audience or scope
// element is an allow-list no token can satisfy, and it would pass the
// disjoint-audience rule for a repeated org, so it is refused where it is written.
func TestValidateIssuersConfigRejectsBlankListElements(t *testing.T) {
	tests := []struct {
		name   string
		issuer IssuerConfig
	}{
		{name: "issuer_audience", issuer: IssuerConfig{Audiences: []string{"api", ""}}},
		{name: "issuer_scope", issuer: IssuerConfig{Scopes: []string{" "}}},
		{name: "mapping_audience", issuer: IssuerConfig{ClaimMappings: []cauth.ClaimMapping{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}, Audiences: []string{""}}}}},
		{name: "mapping_scope", issuer: IssuerConfig{ClaimMappings: []cauth.ClaimMapping{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}, Scopes: []string{"nico:read", ""}}}}},
	}

	c := &Config{v: viper.New()}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tt.issuer.Origin, tt.issuer.Issuer, tt.issuer.JWKS = "custom", "https://idp.acme.com", "https://idp.acme.com/jwks"
			if tt.issuer.ClaimMappings == nil {
				tt.issuer.ClaimMappings = []cauth.ClaimMapping{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}}}
			}

			err := c.ValidateIssuersConfig([]IssuerConfig{tt.issuer})

			require.Error(t, err)
			assert.Contains(t, err.Error(), "must not contain an empty value")
		})
	}
}

func TestValidateIssuersConfigJWKSTimeout(t *testing.T) {
	tests := []struct {
		name        string
		timeout     string
		wantErrPart string
	}{
		{name: "unset_uses_the_default", timeout: ""},
		{name: "typical", timeout: "5s"},
		{name: "at_the_maximum", timeout: MaxJWKSTimeout.String()},
		{name: "unparseable", timeout: "soon", wantErrPart: "invalid jwksTimeout"},
		{name: "zero", timeout: "0s", wantErrPart: "must be positive"},
		{name: "negative", timeout: "-5s", wantErrPart: "must be positive"},
		{name: "above_the_maximum", timeout: (MaxJWKSTimeout + time.Second).String(), wantErrPart: "must not exceed"},
	}

	c := &Config{v: viper.New()}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := c.ValidateIssuersConfig([]IssuerConfig{{
				Origin:        "custom",
				Issuer:        "https://idp.acme.com",
				JWKS:          "https://idp.acme.com/jwks",
				JWKSTimeout:   tt.timeout,
				ClaimMappings: []cauth.ClaimMapping{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}}},
			}})

			if tt.wantErrPart == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErrPart)
		})
	}
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

// TestConfig_GetTracingServiceName proves the binary supplies a service name
// when the config omits or empties it, so an export never lacks service.name.
func TestConfig_GetTracingServiceName(t *testing.T) {
	tcs := []struct {
		descr      string
		configured *string
		want       string
	}{
		{descr: "binary default when omitted", want: DefaultTracingServiceName},
		{descr: "binary default when explicitly empty", configured: new(string), want: DefaultTracingServiceName},
		{descr: "configured name wins", configured: ptr("custom-api"), want: "custom-api"},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			v := viper.New()
			if tc.configured != nil {
				v.Set(ConfigTracingServiceName, *tc.configured)
			}
			cfg := &Config{v: v}
			assert.Equal(t, tc.want, cfg.GetTracingServiceName())
		})
	}
}

func ptr(s string) *string { return &s }
