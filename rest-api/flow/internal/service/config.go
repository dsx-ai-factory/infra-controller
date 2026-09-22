// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"errors"
	"fmt"
	"os"
	"strconv"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/endpoint"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/authz"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/certs"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/clients/temporal"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor"
	pkgcerts "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/certs"
)

const (
	// DefaultPort is the default port the Flow gRPC server listens on.
	DefaultPort = 50051

	// EnvVarName is the environment variable operators set to declare the
	// deployment environment. Valid values: "development", "staging", "production".
	// Must be set explicitly; there is no implicit default.
	EnvVarName = "FLOW_ENV"
)

// deploymentEnv identifies the deployment environment, controlling which
// safety checks are enforced at startup.
type deploymentEnv string

const (
	envDevelopment deploymentEnv = "development"
	envStaging     deploymentEnv = "staging"
	envProduction  deploymentEnv = "production"
)

// GetDeploymentEnv reads FLOW_ENV and returns the resolved environment name.
// An unset or empty value is an error; callers that want a development default
// (e.g. a local CLI entrypoint) must set the variable before calling this.
func GetDeploymentEnv() (string, error) {
	v := os.Getenv(EnvVarName)
	if v == "" {
		return "", fmt.Errorf("%s is required: must be development, staging, or production", EnvVarName)
	}

	switch deploymentEnv(v) {
	case envDevelopment, envStaging, envProduction:
		return v, nil
	default:
		return "", fmt.Errorf("unknown %s value %q: must be development, staging, or production", EnvVarName, v) //nolint:lll
	}
}

// Config holds the service configuration.
// It uses interfaces to abstract implementation details:
//   - ExecutorConfig: abstracts the task executor (e.g., Temporal)
type Config struct {
	Port             int
	DBConf           cdb.Config
	ExecutorConf     executor.ExecutorConfig
	FlowConfig       config.Config
	CMConfig         cmconfig.Config
	ProviderRegistry *providerapi.ProviderRegistry
	// DataCipher protects optional firmware authentication data. When nil,
	// operations without authentication data remain available.
	DataCipher *secret.Cipher

	// DevMode enables developer options such as gRPC reflection and debug
	// logging. Must not be set in staging/production environments.
	DevMode bool

	// CertConfig holds certificate file paths for the gRPC server listener.
	// When set, these take precedence over CERTDIR / the k8s default.
	// Either all three fields must be set or none.
	CertConfig pkgcerts.Config

	// Authorization identifies the mTLS-authenticated services allowed to call
	// Flow. It is required whenever TLS is available.
	Authorization authz.Config
}

// Validate checks the Config for unsafe combinations and returns an error for
// the first violation found, in priority order:
//
//  1. Unknown or unset FLOW_ENV — always rejected regardless of other settings.
//  2. DevMode in a non-development environment — staging and production block it.
//  3. Partial CertConfig — all three cert paths must be set together or not at all.
//  4. Missing TLS in staging or production — those environments require mTLS.
//  5. Secure gRPC without a valid service-identity allowlist.
func (c Config) Validate() error {
	envStr, err := GetDeploymentEnv()
	if err != nil {
		return err
	}

	env := deploymentEnv(envStr)

	// Dev-mode is only allowed in development.
	if c.DevMode && env != envDevelopment {
		return fmt.Errorf("--dev-mode is not allowed in %q environment", env)
	}
	// Reject partial CertConfig before reaching IsTLSAvailable. A
	// partial config would cause IsSet() to return false, letting the CERTDIR /
	// SPIFFE fallback satisfy the TLS check even though those certs would never
	// be used by the server (it would attempt to load the incomplete paths).
	if err := c.CertConfig.Validate(); err != nil {
		return err
	}

	if certs.IsTLSAvailable(c.CertConfig) {
		if err := c.Authorization.Validate(); err != nil {
			return fmt.Errorf("gRPC service authorization: %w", err)
		}

		return nil
	}

	if env == envStaging || env == envProduction {
		return fmt.Errorf("%q environment requires TLS certificates to be present", env)
	}

	if err := c.Authorization.Mode.Validate(); err != nil {
		return fmt.Errorf("gRPC service authorization: %w", err)
	}

	if len(c.Authorization.AllowedServiceIdentities) > 0 ||
		c.Authorization.Mode != authz.ModeAudit {
		return fmt.Errorf("gRPC service authorization requires TLS")
	}

	return nil
}

// BuildTemporalConfigFromEnv builds a Temporal client configuration from
// environment variables: TEMPORAL_HOST, TEMPORAL_PORT, TEMPORAL_NAMESPACE,
// TEMPORAL_CERT_PATH, TEMPORAL_ENABLE_TLS, and TEMPORAL_SERVER_NAME.
func BuildTemporalConfigFromEnv() (*temporal.Config, error) {
	host := os.Getenv("TEMPORAL_HOST")
	if host == "" {
		return nil, errors.New("TEMPORAL_HOST is not set")
	}

	port, err := strconv.Atoi(os.Getenv("TEMPORAL_PORT"))
	if err != nil {
		return nil, errors.New("fail to retrieve port")
	}

	namespace := os.Getenv("TEMPORAL_NAMESPACE")
	if namespace == "" {
		return nil, errors.New("TEMPORAL_NAMESPACE is not set")
	}

	return &temporal.Config{
		Endpoint: endpoint.Config{
			Host:              host,
			Port:              port,
			CACertificatePath: os.Getenv("TEMPORAL_CERT_PATH"),
		},
		EnableTLS:  os.Getenv("TEMPORAL_ENABLE_TLS") == "true",
		Namespace:  namespace,
		ServerName: os.Getenv("TEMPORAL_SERVER_NAME"),
	}, nil
}
