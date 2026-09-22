// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"encoding/base64"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/authz"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TestApplyComputeImplementationOverride covers the env-var fallback
// path that exists for migrating compute between nicolegacy and the new
// Component Manager-based nico implementation. Subsequent catalog
// validation rejects unknown names, so the override here is intentionally
// minimal: it only adjusts the config map.
func TestApplyComputeImplementationOverride(t *testing.T) {
	t.Run("env unset is a no-op", func(t *testing.T) {
		t.Setenv(computeImplEnvVar, "")

		cfg := cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "nicolegacy",
			},
		}

		applyComputeImplementationOverride(&cfg)

		assert.Equal(t, "nicolegacy", cfg.ComponentManagers[devicetypes.ComponentTypeCompute])
	})

	t.Run("whitespace value is treated as unset", func(t *testing.T) {
		t.Setenv(computeImplEnvVar, "   ")

		cfg := cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "nicolegacy",
			},
		}

		applyComputeImplementationOverride(&cfg)

		assert.Equal(t, "nicolegacy", cfg.ComponentManagers[devicetypes.ComponentTypeCompute])
	})

	t.Run("override replaces existing compute selection", func(t *testing.T) {
		t.Setenv(computeImplEnvVar, "nico")

		cfg := cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute:    "nicolegacy",
				devicetypes.ComponentTypeNVSwitch:   "nico",
				devicetypes.ComponentTypePowerShelf: "nico",
			},
		}

		applyComputeImplementationOverride(&cfg)

		assert.Equal(t, "nico", cfg.ComponentManagers[devicetypes.ComponentTypeCompute])
		// Other component types must be untouched.
		assert.Equal(t, "nico", cfg.ComponentManagers[devicetypes.ComponentTypeNVSwitch])
		assert.Equal(t, "nico", cfg.ComponentManagers[devicetypes.ComponentTypePowerShelf])
	})

	t.Run("override surrounding whitespace is trimmed", func(t *testing.T) {
		t.Setenv(computeImplEnvVar, "  nico  ")

		cfg := cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "nicolegacy",
			},
		}

		applyComputeImplementationOverride(&cfg)

		assert.Equal(t, "nico", cfg.ComponentManagers[devicetypes.ComponentTypeCompute])
	})

	t.Run("override initialises map when nil", func(t *testing.T) {
		t.Setenv(computeImplEnvVar, "nico")

		cfg := cmconfig.Config{}

		applyComputeImplementationOverride(&cfg)

		assert.Equal(t, "nico", cfg.ComponentManagers[devicetypes.ComponentTypeCompute])
	})
}

func TestLoadAuthorizationConfig(t *testing.T) {
	originalIdentities := allowedServiceIdentities
	t.Cleanup(func() {
		allowedServiceIdentities = originalIdentities
	})

	plainTextContent := "\n  " + allowedServiceIdentityForTest + "  \n\n"
	commentContent := "# service identities\n"
	tests := map[string]struct {
		cliIdentities     []string
		fileEnvSet        bool
		fileEnvValue      string
		fileContent       *string
		mode              string
		wantConfig        authz.Config
		wantLoadErr       string
		wantValidationErr string
	}{
		"uses CLI identities when file environment variable is unset": {
			cliIdentities: []string{allowedServiceIdentityForTest},
			wantConfig: authz.Config{
				AllowedServiceIdentities: []string{allowedServiceIdentityForTest},
				Mode:                     authz.ModeAudit,
			},
		},
		"rejects blank file environment variable": {
			fileEnvSet:   true,
			fileEnvValue: "   ",
			wantLoadErr:  "read allowed service identities file \"\"",
		},
		"loads plain-text identity list and audit mode": {
			fileContent: &plainTextContent,
			mode:        string(authz.ModeAudit),
			wantConfig: authz.Config{
				AllowedServiceIdentities: []string{allowedServiceIdentityForTest},
				Mode:                     authz.ModeAudit,
			},
		},
		"rejects file and CLI identities together": {
			cliIdentities: []string{allowedServiceIdentityForTest},
			fileEnvSet:    true,
			fileEnvValue:  "identities.txt",
			wantLoadErr:   "cannot be configured by both file and command-line options",
		},
		"does not interpret comments": {
			fileContent: &commentContent,
			wantConfig: authz.Config{
				AllowedServiceIdentities: []string{"# service identities"},
				Mode:                     authz.ModeAudit,
			},
			wantValidationErr: "not a valid SPIFFE ID",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			allowedServiceIdentities = test.cliIdentities
			t.Setenv(authorizationModeEnvVar, test.mode)

			switch {
			case test.fileContent != nil:
				path := filepath.Join(t.TempDir(), "allowed-services.txt")
				require.NoError(t, os.WriteFile(path, []byte(*test.fileContent), 0o600))
				t.Setenv(allowedServiceIdentitiesFileEnvVar, path)
			case test.fileEnvSet:
				t.Setenv(allowedServiceIdentitiesFileEnvVar, test.fileEnvValue)
			default:
				t.Setenv(allowedServiceIdentitiesFileEnvVar, "temporary")
				require.NoError(t, os.Unsetenv(allowedServiceIdentitiesFileEnvVar))
			}

			config, err := loadAuthorizationConfig()
			if test.wantLoadErr != "" {
				require.ErrorContains(t, err, test.wantLoadErr)
				return
			}

			require.NoError(t, err)
			assert.Equal(t, test.wantConfig, config)
			if test.wantValidationErr != "" {
				require.ErrorContains(t, config.Validate(), test.wantValidationErr)
			}
		})
	}
}

const allowedServiceIdentityForTest = "spiffe://example.test/ns/site/sa/site-workflow"

func TestLoadDataCipherFromEnv(t *testing.T) {
	validKey := base64.StdEncoding.EncodeToString(make([]byte, 32))
	emptyKey := ""
	malformedKey := "not-base64"
	tests := []struct {
		name        string
		envSet      bool
		envValue    string
		keyContents *string
		wantErr     string
		wantCipher  bool
	}{
		{name: "missing environment variable"},
		{
			name:    "empty environment variable",
			envSet:  true,
			wantErr: secret.EncryptionKeyPathEnvVar + " is set but empty",
		},
		{
			name:     "missing key file",
			envSet:   true,
			envValue: "/missing/encryption-key",
			wantErr:  "read data encryption key",
		},
		{name: "valid key", keyContents: &validKey, wantCipher: true},
		{
			name:        "empty key",
			keyContents: &emptyKey,
			wantErr:     "data encryption key is empty",
		},
		{
			name:        "malformed key",
			keyContents: &malformedKey,
			wantErr:     "data encryption key must be base64 encoded",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Setenv(secret.EncryptionKeyPathEnvVar, "temporary")
			require.NoError(t, os.Unsetenv(secret.EncryptionKeyPathEnvVar))
			if tt.keyContents != nil {
				path := filepath.Join(t.TempDir(), "encryption-key")
				require.NoError(
					t,
					os.WriteFile(path, []byte(*tt.keyContents), 0o600),
				)
				t.Setenv(secret.EncryptionKeyPathEnvVar, path)
			} else if tt.envSet {
				t.Setenv(secret.EncryptionKeyPathEnvVar, tt.envValue)
			}

			cipher, err := loadDataCipherFromEnv()
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
				require.Nil(t, cipher)
				return
			}

			require.NoError(t, err)
			if tt.wantCipher {
				require.NotNil(t, cipher)
			} else {
				require.Nil(t, cipher)
			}
		})
	}
}
