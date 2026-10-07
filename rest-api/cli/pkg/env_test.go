// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"bytes"
	"os"
	"sort"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// clearAllNicoEnv unsets every known NICO_* env var before a test so that
// the parent process environment can't leak into ApplyEnvOverrides results.
// t.Setenv handles restoration on test exit for whatever the test sets.
func clearAllNicoEnv(t *testing.T) {
	t.Helper()
	for _, name := range KnownEnvVarNames() {
		t.Setenv(name, "")
	}
}

func TestApplyEnvOverrides_AllRegisteredFields(t *testing.T) {
	clearAllNicoEnv(t)

	t.Setenv("NICO_BASE_URL", "https://api.example.com")
	t.Setenv("NICO_ORG", "test-org")
	t.Setenv("NICO_API_NAME", "carbide")
	t.Setenv("NICO_TOKEN", "direct-token")
	t.Setenv("NICO_TOKEN_COMMAND", "/bin/get-token")
	t.Setenv("NICO_TOKEN_URL", "https://auth.example.com/token")
	t.Setenv("NICO_CLIENT_ID", "nico-cli")
	t.Setenv("NICO_CLIENT_SECRET", "shh")
	t.Setenv("NICO_OIDC_USERNAME", "alice")
	t.Setenv("NICO_OIDC_PASSWORD", "swordfish")
	t.Setenv("NICO_OIDC_TOKEN", "oidc-token")
	t.Setenv("NICO_OIDC_REFRESH_TOKEN", "refresh-token")
	t.Setenv("NICO_OIDC_EXPIRES_AT", "2026-12-31T00:00:00Z")
	t.Setenv("NICO_API_KEY", "nvapi-xyz")
	t.Setenv("NICO_AUTHN_URL", "https://authn.example.com/token")
	t.Setenv("NICO_API_KEY_TOKEN", "exchanged-token")

	cfg := &ConfigFile{}
	applied := ApplyEnvOverrides(cfg)

	assert.Equal(t, "https://api.example.com", cfg.API.Base)
	assert.Equal(t, "test-org", cfg.API.Org)
	assert.Equal(t, "carbide", cfg.API.Name)

	assert.Equal(t, "direct-token", cfg.Auth.Token)
	assert.Equal(t, "/bin/get-token", cfg.Auth.TokenCommand)

	require.NotNil(t, cfg.Auth.OIDC)
	assert.Equal(t, "https://auth.example.com/token", cfg.Auth.OIDC.TokenURL)
	assert.Equal(t, "nico-cli", cfg.Auth.OIDC.ClientID)
	assert.Equal(t, "shh", cfg.Auth.OIDC.ClientSecret)
	assert.Equal(t, "alice", cfg.Auth.OIDC.Username)
	assert.Equal(t, "swordfish", cfg.Auth.OIDC.Password)
	assert.Equal(t, "oidc-token", cfg.Auth.OIDC.Token)
	assert.Equal(t, "refresh-token", cfg.Auth.OIDC.RefreshToken)
	assert.Equal(t, "2026-12-31T00:00:00Z", cfg.Auth.OIDC.ExpiresAt)

	require.NotNil(t, cfg.Auth.APIKey)
	assert.Equal(t, "nvapi-xyz", cfg.Auth.APIKey.Key)
	assert.Equal(t, "https://authn.example.com/token", cfg.Auth.APIKey.AuthnURL)
	assert.Equal(t, "exchanged-token", cfg.Auth.APIKey.Token)

	assert.NotEmpty(t, applied)
	for _, o := range applied {
		assert.True(t, o.Applied, "%s should be marked applied", o.Name)
		assert.NotEmpty(t, o.Value, "%s should carry its value", o.Name)
	}
}

func TestApplyEnvOverrides_TrumpsConfigFile(t *testing.T) {
	clearAllNicoEnv(t)

	cfg := &ConfigFile{
		API: ConfigAPI{
			Base: "http://from-config",
			Org:  "config-org",
			Name: "nico",
		},
		Auth: ConfigAuth{
			Token: "config-token",
			OIDC: &ConfigOIDC{
				TokenURL: "http://from-config/token",
				ClientID: "config-client",
			},
		},
	}

	t.Setenv("NICO_BASE_URL", "http://from-env")
	t.Setenv("NICO_ORG", "env-org")
	t.Setenv("NICO_TOKEN", "env-token")
	t.Setenv("NICO_TOKEN_URL", "http://from-env/token")

	ApplyEnvOverrides(cfg)

	assert.Equal(t, "http://from-env", cfg.API.Base, "NICO_BASE_URL must trump api.base")
	assert.Equal(t, "env-org", cfg.API.Org, "NICO_ORG must trump api.org")
	assert.Equal(t, "nico", cfg.API.Name, "unset NICO_API_NAME must leave api.name alone")
	assert.Equal(t, "env-token", cfg.Auth.Token, "NICO_TOKEN must trump auth.token")
	assert.Equal(t, "http://from-env/token", cfg.Auth.OIDC.TokenURL,
		"NICO_TOKEN_URL must trump auth.oidc.token_url")
	assert.Equal(t, "config-client", cfg.Auth.OIDC.ClientID,
		"unset NICO_CLIENT_ID must preserve config-loaded ClientID")
}

func TestApplyEnvOverrides_AuthScriptAlias(t *testing.T) {
	clearAllNicoEnv(t)

	cfg := &ConfigFile{}
	t.Setenv("NICO_AUTH_SCRIPT", "/bin/legacy-token")
	ApplyEnvOverrides(cfg)
	assert.Equal(t, "/bin/legacy-token", cfg.Auth.TokenCommand,
		"NICO_AUTH_SCRIPT alias should set auth.token_command")
}

func TestApplyEnvOverrides_NICO_TOKEN_COMMAND_WinsOverAlias(t *testing.T) {
	// Mirror urfave/cli's "first non-empty entry in EnvVars wins" rule
	// for the --token-command flag (EnvVars: NICO_TOKEN_COMMAND first,
	// NICO_AUTH_SCRIPT second). When both env vars are set the canonical
	// NICO_TOKEN_COMMAND must win, regardless of which path applied it.
	clearAllNicoEnv(t)

	cfg := &ConfigFile{}
	t.Setenv("NICO_AUTH_SCRIPT", "/bin/legacy")
	t.Setenv("NICO_TOKEN_COMMAND", "/bin/preferred")
	ApplyEnvOverrides(cfg)
	assert.Equal(t, "/bin/preferred", cfg.Auth.TokenCommand,
		"NICO_TOKEN_COMMAND must win over the NICO_AUTH_SCRIPT alias to "+
			"match urfave/cli's flag-EnvVars precedence")
}

func TestApplyEnvOverrides_EmptyEnvIsIgnored(t *testing.T) {
	clearAllNicoEnv(t)

	cfg := &ConfigFile{
		API:  ConfigAPI{Base: "http://kept"},
		Auth: ConfigAuth{Token: "kept"},
	}
	t.Setenv("NICO_BASE_URL", "")
	t.Setenv("NICO_TOKEN", "")
	applied := ApplyEnvOverrides(cfg)
	assert.Empty(t, applied)
	assert.Equal(t, "http://kept", cfg.API.Base)
	assert.Equal(t, "kept", cfg.Auth.Token)
}

func TestApplyEnvOverrides_NilCfg(t *testing.T) {
	clearAllNicoEnv(t)
	t.Setenv("NICO_BASE_URL", "http://anywhere")
	out := ApplyEnvOverrides(nil)
	assert.Nil(t, out)
}

func TestEnvOverridesFromEnvironment_ReportsUnappliedFlagOnlyVars(t *testing.T) {
	clearAllNicoEnv(t)

	t.Setenv("NICO_KEYCLOAK_URL", "http://kc.example.com")
	t.Setenv("NICO_KEYCLOAK_REALM", "nico-prod")
	t.Setenv("NICO_BASE_URL", "http://api")

	overrides := EnvOverridesFromEnvironment()
	require.Len(t, overrides, 3)

	byName := map[string]EnvOverride{}
	for _, o := range overrides {
		byName[o.Name] = o
	}

	require.Contains(t, byName, "NICO_KEYCLOAK_URL")
	assert.False(t, byName["NICO_KEYCLOAK_URL"].Applied,
		"NICO_KEYCLOAK_URL is flag-only and should report Applied=false")

	require.Contains(t, byName, "NICO_KEYCLOAK_REALM")
	assert.True(t, byName["NICO_KEYCLOAK_REALM"].Applied,
		"NICO_KEYCLOAK_REALM maps to auth.oidc.realm and should report Applied=true")

	require.Contains(t, byName, "NICO_BASE_URL")
	assert.True(t, byName["NICO_BASE_URL"].Applied,
		"NICO_BASE_URL maps directly to api.base and should report Applied=true")
}

func TestKnownEnvVarDescriptors_DocsEveryRegisteredEntry(t *testing.T) {
	descriptors := KnownEnvVarDescriptors()
	require.NotEmpty(t, descriptors)

	// Every entry must populate Name and ConfigPath; Value stays empty
	// since this surface is for static documentation, not live env state.
	for _, d := range descriptors {
		assert.NotEmpty(t, d.Name)
		assert.NotEmpty(t, d.ConfigPath, "%s missing ConfigPath", d.Name)
		assert.Empty(t, d.Value, "%s should not carry a value here", d.Name)
	}

	// Spot-check that flag-only entries are reported with Applied=false
	// and direct config-field entries with Applied=true.
	byName := map[string]EnvOverride{}
	for _, d := range descriptors {
		byName[d.Name] = d
	}
	require.Contains(t, byName, "NICO_BASE_URL")
	assert.True(t, byName["NICO_BASE_URL"].Applied,
		"direct config field override should report Applied=true")
	require.Contains(t, byName, "NICO_KEYCLOAK_URL")
	assert.False(t, byName["NICO_KEYCLOAK_URL"].Applied,
		"flag-only env var should report Applied=false")
	require.Contains(t, byName, "NICO_TOKEN")
	assert.True(t, byName["NICO_TOKEN"].Sensitive,
		"NICO_TOKEN must stay flagged as sensitive")
}

func TestKnownEnvVarNames_CoversEveryConfigField(t *testing.T) {
	// Sanity: KnownEnvVarNames should include every documented var.
	got := KnownEnvVarNames()
	assert.True(t, sort.StringsAreSorted(got),
		"KnownEnvVarNames must be sorted")

	required := []string{
		// api.* fields.
		"NICO_BASE_URL", "NICO_ORG", "NICO_API_NAME",
		// auth.* top-level.
		"NICO_TOKEN", "NICO_TOKEN_COMMAND",
		// auth.oidc.* fields.
		"NICO_TOKEN_URL", "NICO_CLIENT_ID", "NICO_CLIENT_SECRET",
		"NICO_OIDC_USERNAME", "NICO_OIDC_PASSWORD",
		"NICO_OIDC_TOKEN", "NICO_OIDC_REFRESH_TOKEN", "NICO_OIDC_EXPIRES_AT",
		// auth.api_key.* fields.
		"NICO_API_KEY", "NICO_AUTHN_URL", "NICO_API_KEY_TOKEN",
		// flag-only / config selection.
		"NICO_CONFIG", "NICO_AUTH_SCRIPT", "NICO_KEYCLOAK_URL", "NICO_KEYCLOAK_REALM",
	}
	for _, name := range required {
		assert.Contains(t, got, name, "missing required env var: %s", name)
	}
}

func TestFormatEnvOverrides_MaskingAndPlain(t *testing.T) {
	overrides := []EnvOverride{
		{Name: "NICO_BASE_URL", ConfigPath: "api.base", Value: "https://api.example.com", Applied: true},
		{Name: "NICO_TOKEN", ConfigPath: "auth.token", Value: "eyJhbGciOiJIUzI1NiJ9.abc", Sensitive: true, Applied: true},
		{Name: "NICO_KEYCLOAK_URL", ConfigPath: "(--keycloak-url)", Value: "http://kc", Applied: false},
	}

	plain := FormatEnvOverrides(overrides, false)
	assert.Contains(t, plain, "NICO_BASE_URL")
	assert.Contains(t, plain, "https://api.example.com")
	assert.Contains(t, plain, "eyJhbGciOiJIUzI1NiJ9.abc",
		"plain mode must show full sensitive values when caller asks for it")
	assert.Contains(t, plain, "(sensitive)")
	assert.Contains(t, plain, "[flag-only]",
		"unapplied flag-only env vars should carry the [flag-only] marker")

	masked := FormatEnvOverrides(overrides, true)
	assert.Contains(t, masked, "NICO_BASE_URL")
	assert.NotContains(t, masked, "eyJhbGciOiJIUzI1NiJ9.abc",
		"masked mode must hide the sensitive token")
	assert.Contains(t, masked, "REDACTED")
	assert.Contains(t, masked, "https://api.example.com",
		"non-sensitive values are not masked")

	empty := FormatEnvOverrides(nil, false)
	assert.True(t, strings.Contains(empty, "no NICO_* environment variables set"),
		"empty list should produce a friendly placeholder")
}

func TestPrintEnvOverridesForDebugRedactsSensitiveValues(t *testing.T) {
	clearAllNicoEnv(t)
	t.Setenv("NICO_BASE_URL", "https://api.example.com")
	t.Setenv("NICO_TOKEN", "debug-token-that-must-not-be-printed")

	var output bytes.Buffer
	printEnvOverridesForDebug(&output)

	assert.Contains(t, output.String(), "NICO_BASE_URL")
	assert.Contains(t, output.String(), "https://api.example.com")
	assert.Contains(t, output.String(), "NICO_TOKEN")
	assert.Contains(t, output.String(), "REDACTED")
	assert.NotContains(t, output.String(), "debug-token-that-must-not-be-printed")
}

func TestApplyEnvOverrides_OnceLoadedConfig(t *testing.T) {
	// LoadConfigFromPath plus ApplyEnvOverrides is the production
	// pipeline. This test pins down the contract that callers can use
	// the two together to layer env on top of a YAML config.
	clearAllNicoEnv(t)

	dir := t.TempDir()
	path := dir + "/config.yaml"
	contents := "api:\n  base: http://config-only\n  org: config-org\n  name: nico\n"
	require.NoError(t, os.WriteFile(path, []byte(contents), 0600))

	t.Setenv("NICO_BASE_URL", "http://from-env")

	cfg, err := LoadConfigFromPath(path)
	require.NoError(t, err)
	require.Equal(t, "http://config-only", cfg.API.Base,
		"LoadConfigFromPath must NOT auto-apply env (callers do it explicitly)")

	applied := ApplyEnvOverrides(cfg)
	require.Len(t, applied, 1)
	assert.Equal(t, "http://from-env", cfg.API.Base)
	assert.Equal(t, "config-org", cfg.API.Org,
		"unset NICO_ORG should leave the YAML-loaded value alone")
}
