// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"fmt"
	"os"
	"sort"
	"strings"
)

// EnvOverride describes a single NICO_* environment variable, the config
// field (or flag) it influences, and the value currently set in the process
// environment. Values are returned exactly as os.Getenv reports them; the
// caller is responsible for masking when displaying sensitive data.
type EnvOverride struct {
	// Name is the environment variable name (e.g. "NICO_BASE_URL").
	Name string
	// ConfigPath is a human-readable description of where the value lands.
	// Direct config fields use dotted YAML paths (e.g. "api.base"); env
	// vars that feed only the flag layer carry a parenthesized note instead.
	ConfigPath string
	// Value is the current value of the env var. Empty for unset env vars.
	Value string
	// Sensitive marks values that should be masked in casual display
	// (tokens, passwords, client secrets, NGC API keys).
	Sensitive bool
	// Applied is true when ApplyEnvOverrides actually wrote this value
	// back into the ConfigFile. Env vars that only feed flags
	// (NICO_CONFIG, NICO_KEYCLOAK_URL) are reported but not applied here.
	Applied bool
}

// envOverrideEntry is the static description of a known NICO_* env var.
// apply may be nil for env vars that affect only the flag layer
// (NICO_CONFIG, NICO_KEYCLOAK_URL); they are still reported by
// EnvOverridesFromEnvironment but ApplyEnvOverrides skips them.
type envOverrideEntry struct {
	name       string
	configPath string
	sensitive  bool
	apply      func(cfg *ConfigFile, value string)
}

// envOverrideRegistry is the canonical list of every environment variable
// nicocli understands. Keep this list in sync with cli/README.md and with
// the urfave/cli flag EnvVars in app.go and auth.go: any flag's EnvVars
// entry should also appear here so that the same variable also propagates
// to fields that are only addressable through the config file (e.g. when
// no matching flag exists, or when the value should reach config-only
// callers like the TUI).
var envOverrideRegistry = []envOverrideEntry{
	// Config path override (handled by the global --config flag).
	{
		name:       "NICO_CONFIG",
		configPath: "(--config; selects config file path)",
	},

	// api.* fields.
	{
		name:       "NICO_BASE_URL",
		configPath: "api.base",
		apply:      func(cfg *ConfigFile, v string) { cfg.API.Base = v },
	},
	{
		name:       "NICO_ORG",
		configPath: "api.org",
		apply:      func(cfg *ConfigFile, v string) { cfg.API.Org = v },
	},
	{
		name:       "NICO_API_NAME",
		configPath: "api.name",
		apply:      func(cfg *ConfigFile, v string) { cfg.API.Name = v },
	},

	// auth.* top-level fields. Order between NICO_TOKEN_COMMAND and the
	// NICO_AUTH_SCRIPT alias matters: when both are set, urfave/cli takes
	// the first non-empty entry in the flag's EnvVars list (currently
	// "NICO_TOKEN_COMMAND" then "NICO_AUTH_SCRIPT"), so NICO_TOKEN_COMMAND
	// must win. We achieve that by listing the alias first and the
	// canonical name second, so the canonical apply runs last and
	// overwrites the alias's apply.
	{
		name:       "NICO_TOKEN",
		configPath: "auth.token",
		sensitive:  true,
		apply:      func(cfg *ConfigFile, v string) { cfg.Auth.Token = v },
	},
	{
		name:       "NICO_AUTH_SCRIPT",
		configPath: "auth.token_command",
		apply:      func(cfg *ConfigFile, v string) { cfg.Auth.TokenCommand = v },
	},
	{
		name:       "NICO_TOKEN_COMMAND",
		configPath: "auth.token_command",
		apply:      func(cfg *ConfigFile, v string) { cfg.Auth.TokenCommand = v },
	},

	// auth.oidc.* fields.
	{
		name:       "NICO_TOKEN_URL",
		configPath: "auth.oidc.token_url",
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).TokenURL = v },
	},
	{
		name:       "NICO_CLIENT_ID",
		configPath: "auth.oidc.client_id",
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).ClientID = v },
	},
	{
		name:       "NICO_CLIENT_SECRET",
		configPath: "auth.oidc.client_secret",
		sensitive:  true,
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).ClientSecret = v },
	},
	{
		name:       "NICO_OIDC_USERNAME",
		configPath: "auth.oidc.username",
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).Username = v },
	},
	{
		name:       "NICO_OIDC_PASSWORD",
		configPath: "auth.oidc.password",
		sensitive:  true,
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).Password = v },
	},
	{
		name:       "NICO_OIDC_TOKEN",
		configPath: "auth.oidc.token",
		sensitive:  true,
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).Token = v },
	},
	{
		name:       "NICO_OIDC_REFRESH_TOKEN",
		configPath: "auth.oidc.refresh_token",
		sensitive:  true,
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).RefreshToken = v },
	},
	{
		name:       "NICO_OIDC_EXPIRES_AT",
		configPath: "auth.oidc.expires_at",
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).ExpiresAt = v },
	},

	// auth.api_key.* fields.
	{
		name:       "NICO_API_KEY",
		configPath: "auth.api_key.key",
		sensitive:  true,
		apply:      func(cfg *ConfigFile, v string) { ensureAPIKey(cfg).Key = v },
	},
	{
		name:       "NICO_AUTHN_URL",
		configPath: "auth.api_key.authn_url",
		apply:      func(cfg *ConfigFile, v string) { ensureAPIKey(cfg).AuthnURL = v },
	},
	{
		name:       "NICO_API_KEY_TOKEN",
		configPath: "auth.api_key.token",
		sensitive:  true,
		apply:      func(cfg *ConfigFile, v string) { ensureAPIKey(cfg).Token = v },
	},

	// NICO_KEYCLOAK_URL has no config field of its own: it exists only to build
	// auth.oidc.token_url at login, and that is what gets persisted. Surface it so
	// users still see it in --debug and the TUI env command.
	{
		name:       "NICO_KEYCLOAK_URL",
		configPath: "(--keycloak-url; constructs auth.oidc.token_url at login)",
	},
	{
		name:       "NICO_KEYCLOAK_REALM",
		configPath: "auth.oidc.realm",
		apply:      func(cfg *ConfigFile, v string) { ensureOIDC(cfg).Realm = v },
	},
}

func ensureOIDC(cfg *ConfigFile) *ConfigOIDC {
	if cfg.Auth.OIDC == nil {
		cfg.Auth.OIDC = &ConfigOIDC{}
	}
	return cfg.Auth.OIDC
}

func ensureAPIKey(cfg *ConfigFile) *ConfigAPIKey {
	if cfg.Auth.APIKey == nil {
		cfg.Auth.APIKey = &ConfigAPIKey{}
	}
	return cfg.Auth.APIKey
}

// ApplyEnvOverrides reads every NICO_* env var from the process
// environment and writes its value into the matching ConfigFile field,
// overriding whatever was loaded from the config YAML. It returns the list
// of overrides that were actually applied (apply func != nil and env var
// was set to a non-empty string), in registry order.
//
// Env vars that influence only the flag layer (e.g. NICO_KEYCLOAK_URL) are
// not applied here -- urfave/cli reads them via the flag's EnvVars list.
// Use EnvOverridesFromEnvironment to get every NICO_* env var that is set,
// applied or not.
func ApplyEnvOverrides(cfg *ConfigFile) []EnvOverride {
	if cfg == nil {
		return nil
	}
	var out []EnvOverride
	for _, entry := range envOverrideRegistry {
		v, ok := os.LookupEnv(entry.name)
		if !ok || v == "" {
			continue
		}
		applied := false
		if entry.apply != nil {
			entry.apply(cfg, v)
			applied = true
		}
		out = append(out, EnvOverride{
			Name:       entry.name,
			ConfigPath: entry.configPath,
			Value:      v,
			Sensitive:  entry.sensitive,
			Applied:    applied,
		})
	}
	return out
}

// EnvOverridesFromEnvironment returns every NICO_* env var that is set
// (to a non-empty value) in the current process environment, regardless
// of whether ApplyEnvOverrides would write it back to ConfigFile. Useful
// for the --debug listing and the interactive `env` command.
func EnvOverridesFromEnvironment() []EnvOverride {
	var out []EnvOverride
	for _, entry := range envOverrideRegistry {
		v, ok := os.LookupEnv(entry.name)
		if !ok || v == "" {
			continue
		}
		out = append(out, EnvOverride{
			Name:       entry.name,
			ConfigPath: entry.configPath,
			Value:      v,
			Sensitive:  entry.sensitive,
			Applied:    entry.apply != nil,
		})
	}
	return out
}

// KnownEnvVarNames returns every NICO_* env var nicocli recognizes,
// sorted alphabetically. Useful for tests and documentation generation.
func KnownEnvVarNames() []string {
	seen := map[string]struct{}{}
	for _, entry := range envOverrideRegistry {
		seen[entry.name] = struct{}{}
	}
	names := make([]string, 0, len(seen))
	for n := range seen {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

// KnownEnvVarDescriptors returns metadata for every NICO_* env var
// nicocli recognizes, in registry order, with Value left empty and
// Applied set to true for direct config-field overrides (false for
// flag-only entries like NICO_KEYCLOAK_URL). Use this for static
// documentation surfaces (help output, generated docs); use
// EnvOverridesFromEnvironment when you want the values currently set
// in the process environment.
func KnownEnvVarDescriptors() []EnvOverride {
	out := make([]EnvOverride, 0, len(envOverrideRegistry))
	for _, entry := range envOverrideRegistry {
		out = append(out, EnvOverride{
			Name:       entry.name,
			ConfigPath: entry.configPath,
			Sensitive:  entry.sensitive,
			Applied:    entry.apply != nil,
		})
	}
	return out
}

// FormatEnvOverrides returns a multiline string describing each override
// for human display. Layout:
//
//	NICO_BASE_URL=http://localhost:8388  -> api.base
//	NICO_TOKEN=eyJh...REDACTED            -> auth.token (sensitive)
//
// When mask is true, sensitive values are replaced with a short prefix
// followed by "...REDACTED". When mask is false, full values are printed.
// The final line is terminated with a newline.
func FormatEnvOverrides(overrides []EnvOverride, mask bool) string {
	if len(overrides) == 0 {
		return "(no NICO_* environment variables set)\n"
	}
	maxName := 0
	for _, o := range overrides {
		if l := len(o.Name); l > maxName {
			maxName = l
		}
	}
	var b strings.Builder
	for _, o := range overrides {
		val := o.Value
		if mask && o.Sensitive {
			val = maskSensitive(val)
		}
		marker := ""
		if !o.Applied {
			marker = " [flag-only]"
		}
		sensitiveTag := ""
		if o.Sensitive {
			sensitiveTag = " (sensitive)"
		}
		fmt.Fprintf(&b, "%-*s = %s  -> %s%s%s\n",
			maxName, o.Name, val, o.ConfigPath, sensitiveTag, marker)
	}
	return b.String()
}

// maskSensitive returns a redacted form of a sensitive value: the first
// six characters followed by "...REDACTED". Values shorter than seven
// characters are fully redacted.
func maskSensitive(v string) string {
	if len(v) <= 6 {
		return "REDACTED"
	}
	return v[:6] + "...REDACTED"
}
