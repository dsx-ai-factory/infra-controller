// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"errors"
	"fmt"
	"log/slog"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	"github.com/fsnotify/fsnotify"
	"github.com/spf13/viper"
)

var (
	_, cur, _, _ = runtime.Caller(0)
	// ProjectRoot describes the folder path of this project
	ProjectRoot = filepath.Join(filepath.Dir(cur), "../..")
)

const DefaultSitePhoneHomeUrl = "http://169.254.169.254/latest/meta-data/phone_home"

const (
	// ConfigFilePath specifies the path to the config file, this contains the default path
	ConfigFilePath = "config.file"
	// ConfigFilePathEnv specifies the environment variable name for the config file path
	ConfigFilePathEnv = "CONFIG_FILE_PATH"

	// ConfigEnvDev specifies if the service is running in development mode
	ConfigEnvDev = "env.dev"
	// ConfigEnvDisconnected specifies if the service is running in disconnected mode
	ConfigEnvDisconnected = "env.disconnected"

	// ConfigAPIName specifies the name of the API
	ConfigAPIName = "api.name"
	// ConfigAPIRouteVersion specifies the version of the API
	ConfigAPIRouteVersion = "api.route.version"

	// ConfigLogLevel specifies the log level
	ConfigLogLevel = "log.level"

	// ConfigSentryDSN is the DSN for Sentry
	ConfigSentryDSN = "log.sentry.dsn"

	// ConfigDBHost specifies the host of the database
	ConfigDBHost = "db.host"
	// ConfigDBPort specifies the port of the database
	ConfigDBPort = "db.port"
	// ConfigDBName specifies the name of the database
	ConfigDBName = "db.name"
	// ConfigDBUser specifies the user of the database
	ConfigDBUser = "db.user"
	// ConfigDBPasswordPath specifies the path for file containing password of the database
	ConfigDBPasswordPath = "db.passwordPath"
	// ConfigDBPassword specifies the password of the database, not read from config file, rather from file specified by db.passwordPath
	ConfigDBPassword = "db.password"

	// ConfigTemporalHost specifies the hostname for Temporal
	ConfigTemporalHost = "temporal.host"
	// ConfigTemporalPort specifies the port for Temporal
	ConfigTemporalPort = "temporal.port"
	// ConfigTemporalServerName specifies the server name for Temporal
	ConfigTemporalServerName = "temporal.serverName"
	// ConfigTemporalNamespace specifies the namespace for Temporal
	ConfigTemporalNamespace = "temporal.namespace"
	// ConfigTemporalQueue specifies the queue for Temporal
	ConfigTemporalQueue = "temporal.queue"
	// ConfigTemporalTlsEnabled specifies if TLS should be used with Temporal
	ConfigTemporalTlsEnabled = "temporal.tls.enabled"
	// ConfigTemporalCertPath specifies the path for client cert
	ConfigTemporalCertPath = "temporal.tls.certPath"
	// ConfigTemporalKeyPath specifies the path for client key
	ConfigTemporalKeyPath = "temporal.tls.keyPath"
	// ConfigTemporalCaPath specifies the path for server CA cert
	ConfigTemporalCaPath = "temporal.tls.caPath"
	// ConfigTemporalEncryptionKey specifies the data encryption key for Temporal
	ConfigTemporalEncryptionKey = "temporal.encryptionKey"
	// ConfigTemporalEncryptionKeyPath specifies the path for file containing data encryption key for Temporal
	ConfigTemporalEncryptionKeyPath = "temporal.encryptionKeyPath"

	// ConfigSiteManagerEnabled is a feature flag for site manager
	ConfigSiteManagerEnabled = "siteManager.enabled"
	// ConfigSiteManagerEndpoint is the service endpoint for site manager
	ConfigSiteManagerEndpoint = "siteManager.svcEndpoint"

	// ConfigSitePhoneHomeUrl is a phone home url from the site
	ConfigSitePhoneHomeUrl = "site.phoneHomeUrl"

	// ConfigMetricsEnabled is a feature flag for Prometheus metrics
	ConfigMetricsEnabled = "metrics.enabled"
	// ConfigMetricsPort specifies the port for Prometheus metrics
	ConfigMetricsPort = "metrics.port"
	// ConfigMetricsNamespace specifies the prefix for every exposed metric name
	ConfigMetricsNamespace = "metrics.namespace"

	// ConfigTracingEnabled is a feature flag for tracing
	ConfigTracingEnabled = "tracing.enabled"
	// ConfigTracingServiceName is the name of the tracing service
	ConfigTracingServiceName = "tracing.serviceName"

	// ConfigKeycloakEnabled is a feature flag for Keycloak authentication
	ConfigKeycloakEnabled = "keycloak.enabled"
	// ConfigKeycloakBaseURL is the base URL of the Keycloak server
	ConfigKeycloakBaseURL = "keycloak.baseURL"
	// ConfigKeycloakExternalBaseURL is the external base URL of the Keycloak server
	ConfigKeycloakExternalBaseURL = "keycloak.externalBaseURL"
	// ConfigKeycloakRealm is the Keycloak realm name
	ConfigKeycloakRealm = "keycloak.realm"
	// ConfigKeycloakClientID is the Keycloak client ID
	ConfigKeycloakClientID = "keycloak.clientID"
	// ConfigKeycloakClientSecretPath is the path to the file containing the client secret
	ConfigKeycloakClientSecretPath = "keycloak.clientSecretPath"
	// ConfigKeycloakClientSecret is the client secret, read from the secret file
	ConfigKeycloakClientSecret = "keycloak.clientSecret"
	// ConfigKeycloakServiceAccountEnabled is a feature flag for service account support
	ConfigKeycloakServiceAccountEnabled = "keycloak.serviceAccount"

	// ConfigRateLimiterEnabled is a feature flag for rate limiter
	ConfigRateLimiterEnabled = "rateLimiter.enabled"
	// ConfigRateLimiterRate specifies the rate limit (requests per second)
	ConfigRateLimiterRate = "rateLimiter.rate"
	// ConfigRateLimiterBurst specifies the burst size
	ConfigRateLimiterBurst = "rateLimiter.burst"
	// ConfigRateLimiterExpiresIn specifies the expiration time in seconds
	ConfigRateLimiterExpiresIn = "rateLimiter.expiresIn"

	// ConfigDPSEnabled controls whether NICo makes direct DPS calls.
	ConfigDPSEnabled = "powerProvisioning.dps.enabled"
	// ConfigDPSEndpoint is the remote DPS gRPC endpoint.
	ConfigDPSEndpoint = "powerProvisioning.dps.endpoint"
	// ConfigDPSRequestTimeout bounds each direct DPS operation.
	ConfigDPSRequestTimeout = "powerProvisioning.dps.requestTimeout"
	// ConfigDPSTokenPath specifies the bearer-token secret file.
	ConfigDPSTokenPath = "powerProvisioning.dps.tokenPath"
	// ConfigDPSCAPath specifies the CA bundle used to authenticate DPS.
	ConfigDPSCAPath = "powerProvisioning.dps.tls.caPath"
	// ConfigDPSServerName optionally overrides TLS server-name verification.
	ConfigDPSServerName = "powerProvisioning.dps.tls.serverName"
)

// IssuerConfig represents a single issuer configuration entry
// This is the preferred configuration format that supports claim mappings
type IssuerConfig struct {
	Name                         string               `mapstructure:"name"`
	Origin                       string               `mapstructure:"origin"` // String: "kas-legacy", "kas-ssa", "keycloak", "custom", "kas"
	JWKS                         string               `mapstructure:"jwks"`
	Issuer                       string               `mapstructure:"issuer"`
	ServiceAccount               bool                 `mapstructure:"serviceAccount"`
	Audiences                    []string             `mapstructure:"audiences"`
	Scopes                       []string             `mapstructure:"scopes"`
	JWKSTimeout                  string               `mapstructure:"jwksTimeout"` // e.g. "5s", "1m"
	ClaimMappings                []cauth.ClaimMapping `mapstructure:"claimMappings"`
	AllowDuplicateStaticOrgNames bool                 `mapstructure:"allowDuplicateStaticOrgNames"` // When true, allows duplicate static org names across issuers
}

// GetOrigin parses the origin and returns it as a string constant
func (ic *IssuerConfig) GetOrigin() (string, error) {
	return ParseOriginString(ic.Origin)
}

// GetJWKSTimeout parses and returns the JWKS timeout duration
func (ic *IssuerConfig) GetJWKSTimeout() (time.Duration, error) {
	if ic.JWKSTimeout == "" {
		return 0, nil // Use default
	}
	return time.ParseDuration(ic.JWKSTimeout)
}

// GetAllowDuplicateStaticOrgNames returns whether duplicate static org names are allowed
// Defaults to false (duplicates not allowed) if not specified
func (ic *IssuerConfig) GetAllowDuplicateStaticOrgNames() bool {
	return ic.AllowDuplicateStaticOrgNames
}

// ParseOriginString converts a string origin to its string constant
func ParseOriginString(origin string) (string, error) {
	normalized := strings.ToLower(origin)
	if normalized == "" {
		return cauth.TokenOriginCustom, nil
	}
	if slices.Contains(cauth.AllowedOrigins, normalized) {
		return normalized, nil
	}
	return "", fmt.Errorf("unknown origin: %s", origin)
}

// RateLimiterConfig holds configuration for rate limiting
type RateLimiterConfig struct {
	Enabled   bool
	Rate      float64 // requests per second
	Burst     int     // maximum burst size
	ExpiresIn int     // expiration time in seconds
}

// Maintain a global config object
var config *Config

// Config represents configurations for the service
type Config struct {
	sync.RWMutex
	v                 *viper.Viper
	db                *cconfig.DBConfig
	temporal          *cconfig.TemporalConfig
	TokenOriginConfig *cauth.TokenOriginConfig
	SiteConfig        *SiteConfig
	KeycloakConfig    *cauth.KeycloakConfig
}

// NewConfig creates a new config object
func NewConfig() *Config {
	if config != nil {
		return config
	}

	c := Config{
		v: newViper(),
	}

	// Set defaults
	c.v.SetDefault(ConfigLogLevel, "info")
	c.v.SetDefault(ConfigEnvDev, false)
	c.v.SetDefault(ConfigEnvDisconnected, false)

	// Set API name
	c.v.SetDefault(ConfigAPIName, "nico")
	c.v.SetDefault(ConfigAPIRouteVersion, "v1")

	// Set config file
	// Check environment variable. If not set, use default
	defaultPath := ProjectRoot + "/config.yaml"
	if os.Getenv(ConfigFilePathEnv) != "" {
		c.v.SetDefault(ConfigFilePath, os.Getenv(ConfigFilePathEnv))
	} else {
		log.Warn().Msg("config file path not set, using default")
		c.v.SetDefault(ConfigFilePath, defaultPath)
	}

	c.v.SetDefault(ConfigDBHost, "localhost")
	c.v.SetDefault(ConfigDBPort, 5432)
	c.v.SetDefault(ConfigTemporalHost, "localhost")
	c.v.SetDefault(ConfigTemporalPort, 7233)
	c.v.SetDefault(ConfigTemporalTlsEnabled, true)

	c.v.SetDefault(ConfigMetricsEnabled, true)
	c.v.SetDefault(ConfigMetricsPort, 9360)
	c.v.SetDefault(ConfigMetricsNamespace, DefaultMetricsNamespace)

	c.v.SetDefault(ConfigTracingEnabled, false)

	// SiteConfig default phone home url
	c.v.SetDefault(ConfigSitePhoneHomeUrl, DefaultSitePhoneHomeUrl)

	// Keycloak needs to be explicitly enabled via config
	c.v.SetDefault(ConfigKeycloakEnabled, false)
	// Service account support is disabled by default
	c.v.SetDefault(ConfigKeycloakServiceAccountEnabled, false)

	// Rate limiter is disabled by default
	c.v.SetDefault(ConfigRateLimiterEnabled, false)
	c.v.SetDefault(ConfigRateLimiterRate, 10.0)     // 10 requests per second
	c.v.SetDefault(ConfigRateLimiterBurst, 30)      // burst of 30 requests
	c.v.SetDefault(ConfigRateLimiterExpiresIn, 180) // 180 seconds (3 minutes)

	c.v.SetDefault(ConfigDPSEnabled, false)
	c.v.SetDefault(ConfigDPSRequestTimeout, 15*time.Second)

	c.v.AutomaticEnv()
	c.v.SetConfigFile(c.GetPathToConfig())

	log.Warn().Str("config.file", c.GetPathToConfig()).Msg("loading config")

	err := c.v.ReadInConfig() // Find and read the config file
	if _, ok := err.(*os.PathError); ok {
		log.Warn().Msgf("no config file '%s' not found. Using default values", c.GetPathToConfig())
	} else if err != nil { // Handle other errors that occurred while reading the config file
		log.Panic().Err(err).Msgf("fatal error while reading the config file: %s", err)
	}
	c.SetSitePhoneHomeUrl(c.v.GetString(ConfigSitePhoneHomeUrl))

	// Set values
	c.setLogLevel()

	if c.GetDBPasswordPath() != "" {
		c.setDBPassword()
	}

	if c.GetKeycloakEnabled() {
		if c.GetKeycloakClientSecretPath() != "" {
			log.Info().Str("keycloak.clientSecretPath", c.GetKeycloakClientSecretPath()).Msg("Keycloak is enabled, reading client secret")
			c.SetKeycloakClientSecret()
		} else {
			log.Panic().Msg("Keycloak is enabled but client secret path is not set")
		}
	} else {
		log.Info().Bool("keycloak.enabled", c.GetKeycloakEnabled()).Msg("Keycloak is disabled")
	}

	if c.GetTemporalEncryptionKey() == "" {
		if c.GetTemporalEncryptionKeyPath() != "" {
			c.SetTemporalEncryptionKey()
		} else {
			log.Panic().Msg("Temporal encryption key path is not set")
		}
	}

	// Validate config
	c.Validate()

	// Watch secret files
	c.WatchSecretFilePaths()
	c.WatchConfigFile()

	config = &c

	return config
}

// Validate validates the config
func (c *Config) Validate() {
	// Check configs that are essential but were not set
	if c.GetDBName() == "" {
		log.Panic().Msg("DB name config must be specified")
	}

	if c.GetDBUser() == "" {
		log.Panic().Msg("DB user config must be specified")
	}

	if value := os.Getenv("NO_DB_PASSWORD_OK"); strings.EqualFold(value, "true") {
		log.Info().Msg("connecting to DB without password")
	} else {
		if c.GetDBPassword() == "" && c.GetDBPasswordPath() == "" {
			log.Panic().Msg("DB password or password path config must be specified")
		}
	}

	if c.GetTemporalTlsEnabled() {
		if c.GetTemporalCertPath() == "" {
			log.Panic().Msg("Temporal cert path config must be specified")
		}

		if c.GetTemporalKeyPath() == "" {
			log.Panic().Msg("Temporal key path config must be specified")
		}

		if c.GetTemporalCaPath() == "" {
			log.Panic().Msg("Temporal CA path config must be specified")
		}
	}

	if c.GetTemporalEncryptionKey() == "" {
		log.Error().Msg("temporal encryption key config was not specified, arguments won't be encrypted")
	}

	// SiteConfig validations
	if err := c.ValidateSiteConfig(); err != nil {
		log.Panic().Err(err).Msg("SiteConfig must be specified")
	}

	// Parse and validate the configured issuers before the auth mode checks below
	issuersConfig, err := c.GetIssuersConfig()
	if err != nil {
		log.Panic().Err(err).Msg("Invalid issuers configuration")
	}
	if len(issuersConfig) > 0 {
		err = c.ValidateIssuersConfig(issuersConfig)
		if err != nil {
			log.Panic().Err(err).Msg("Invalid issuers configuration")
		}
	}

	keycloakEnabled := c.GetKeycloakEnabled()
	// Keycloak validations
	if err := c.ValidateKeycloakConfig(); err != nil {
		log.Panic().Err(err).Msg("Keycloak config must be specified")
	}

	err = c.ValidatePowerProvisioningConfig()
	if err != nil {
		log.Panic().Err(err).Msg("Power provisioning config is invalid")
	}

	if len(issuersConfig) == 0 && !keycloakEnabled {
		log.Panic().Msg("No JWT issuer based auth configured and Keycloak is disabled")
	} else if len(issuersConfig) > 0 && keycloakEnabled {
		log.Panic().Msg("KeyCloak is enabled, we cannot support any JWT issuer based configuration")
	} else if len(issuersConfig) > 0 {
		log.Info().Msgf("Loaded a total of %d JWT issuer based auth configuration", len(issuersConfig))
	} else if keycloakEnabled {
		log.Info().Msg("Keycloak authentication is enabled")
	}
}

// Config setters
// setLogLevel sets the log level
func (c *Config) setLogLevel() {
	logLevel := c.GetLogLevel()
	log.Warn().Str("level", logLevel).Msg("setting log level")
	level, err := zerolog.ParseLevel(logLevel)
	if err != nil {
		log.Err(err).Str("level", logLevel).Msg("failed to parse log level from config")
	}
	zerolog.SetGlobalLevel(level)
}

// setDBPassword sets the password for the database
func (c *Config) setDBPassword() {
	log.Warn().Str("db.passwordPath", c.GetDBPasswordPath()).Msg("setting password by reading from password path")

	passwordBytes, err := os.ReadFile(c.GetDBPasswordPath())
	if err != nil {
		log.Err(err).Str("db.passwordPath", c.GetDBPasswordPath()).Msg("failed to read password from file")
	}
	c.Lock()
	defer c.Unlock()
	c.v.Set(ConfigDBPassword, string(passwordBytes))
}

/* Get sub-configurations */
// GetDBConfig returns the database config
func (c *Config) GetDBConfig() *cconfig.DBConfig {
	if c.db == nil {
		c.db = cconfig.NewDBConfig(c.GetDBHost(), c.GetDBPort(), c.GetDBName(), c.GetDBUser(), c.GetDBPassword())
	}
	return c.db
}

// GetTemporalConfig returns the temporal config
func (c *Config) GetTemporalConfig() (*cconfig.TemporalConfig, error) {
	var err error
	if c.temporal == nil {
		c.temporal, err = cconfig.NewTemporalConfig(c.GetTemporalHost(), c.GetTemporalPort(), c.GetTemporalServerName(), c.GetTemporalNamespace(), c.GetTemporalQueue(), c.GetTemporalEncryptionKey(), c.GetTemporalTlsEnabled(), c.GetTemporalCertPath(), c.GetTemporalKeyPath(), c.GetTemporalCaPath())
	}
	return c.temporal, err
}

// GetOrInitTokenOriginConfig returns the token origin config with all configured auth providers
func (c *Config) GetOrInitTokenOriginConfig() *cauth.TokenOriginConfig {
	if c.TokenOriginConfig == nil {
		c.TokenOriginConfig = cauth.NewTokenOriginConfig()
		if c.GetMetricsEnabled() {
			c.TokenOriginConfig.MetricsNamespace = c.GetMetricsNamespace()
		}

		// Issuers were already validated when the config was loaded
		issuersConfig, err := c.GetIssuersConfig()
		if err != nil {
			log.Panic().Err(err).Msg("Invalid issuers configuration")
		}

		// First pass: collect all static org names (lowercased) from all issuers
		reservedOrgNames := make(map[string]bool)
		for _, issuerCfg := range issuersConfig {
			for _, mapping := range issuerCfg.ClaimMappings {
				if mapping.OrgName != "" {
					reservedOrgNames[strings.ToLower(mapping.OrgName)] = true
				}
			}
		}

		// Second pass: create jwksConfigs and assign reservedOrgNames only to those with dynamic mappings
		for _, issuerCfg := range issuersConfig {
			origin, _ := issuerCfg.GetOrigin() // Already validated
			jwksTimeout, _ := issuerCfg.GetJWKSTimeout()

			// Normalize org names in claim mappings and check for dynamic mappings
			normalizedMappings := make([]cauth.ClaimMapping, len(issuerCfg.ClaimMappings))
			hasDynamicMapping := false
			for i, mapping := range issuerCfg.ClaimMappings {
				normalizedMappings[i] = mapping
				if mapping.OrgName != "" {
					normalizedMappings[i].OrgName = strings.ToLower(mapping.OrgName)
				}
				if mapping.OrgAttribute != "" {
					hasDynamicMapping = true
				}
			}

			jwksCfg := cauth.NewJwksConfig(
				issuerCfg.Name,
				issuerCfg.JWKS,
				issuerCfg.Issuer,
				origin,
				issuerCfg.ServiceAccount,
				issuerCfg.Audiences,
				issuerCfg.Scopes,
			)
			jwksCfg.JWKSTimeout = jwksTimeout
			jwksCfg.ClaimMappings = normalizedMappings

			// Only assign reservedOrgNames to configs with dynamic claim mappings
			if hasDynamicMapping {
				jwksCfg.ReservedOrgNames = reservedOrgNames
			}

			c.TokenOriginConfig.AddJwksConfig(jwksCfg)
		}

		// Add Keycloak configuration if enabled
		if c.GetKeycloakEnabled() {
			keycloakConfig, err := c.GetOrInitKeycloakConfig()
			if err != nil {
				log.Warn().Err(err).Msg("Failed to get Keycloak config, skipping Keycloak JWT origin")
			} else {
				jwksConfig, err := keycloakConfig.GetJwksConfig()
				if err != nil {
					log.Warn().Err(err).Msg("Failed to initialize Keycloak JWKS; continuing with an empty key cache")
				}
				if jwksConfig != nil {
					c.TokenOriginConfig.AddJwksConfig(jwksConfig)
				}
			}
		}

		// Initialize JWKS data
		if err := c.TokenOriginConfig.UpdateAllJWKS(); err != nil {
			log.Warn().Err(err).Msg("Failed to update JWKS data")
		} else {
			log.Info().Msg("Successfully updated JWKS data")
		}
	}

	return c.TokenOriginConfig
}

// GetSiteConfig returns the Site config
func (c *Config) GetSiteConfig() *SiteConfig {
	if c.SiteConfig == nil {
		c.SiteConfig = NewSiteConfig(c.GetEnvDisconnected(), c.GetSitePhoneHomeUrl())
	}

	return c.SiteConfig
}

// GetMetricsConfig returns the Metrics config
func (c *Config) GetMetricsConfig() *MetricsConfig {
	return NewMetricsConfig(c.GetMetricsEnabled(), c.GetMetricsPort(), c.GetMetricsNamespace())
}

// GetRateLimiterConfig returns the rate limiter config
func (c *Config) GetRateLimiterConfig() *RateLimiterConfig {
	return NewRateLimiterConfig(c.GetRateLimiterEnabled(), c.GetRateLimiterRate(), c.GetRateLimiterBurst(), c.GetRateLimiterExpiresIn())
}

// GetDPSConfig returns connection settings for direct DPS calls.
func (c *Config) GetDPSConfig() dpsclient.Config {
	return dpsclient.Config{
		Endpoint:       c.v.GetString(ConfigDPSEndpoint),
		RequestTimeout: c.v.GetDuration(ConfigDPSRequestTimeout),
		TokenPath:      c.v.GetString(ConfigDPSTokenPath),
		CAPath:         c.v.GetString(ConfigDPSCAPath),
		ServerName:     c.v.GetString(ConfigDPSServerName),
	}
}

// ValidatePowerProvisioningConfig validates settings required for direct DPS
// integration. DPS connection fields are ignored when integration is disabled.
func (c *Config) ValidatePowerProvisioningConfig() error {
	enabled, err := c.validateDPSEnabled()
	if err != nil {
		return err
	}
	if !enabled {
		return nil
	}

	dpsConfig := c.GetDPSConfig()
	if strings.TrimSpace(dpsConfig.Endpoint) == "" {
		return fmt.Errorf("powerProvisioning.dps.endpoint is required when DPS integration is enabled")
	}
	if strings.TrimSpace(dpsConfig.TokenPath) == "" {
		return fmt.Errorf("powerProvisioning.dps.tokenPath is required when DPS integration is enabled")
	}
	if strings.TrimSpace(dpsConfig.CAPath) == "" {
		return fmt.Errorf("powerProvisioning.dps.tls.caPath is required when DPS integration is enabled")
	}
	if dpsConfig.RequestTimeout <= 0 {
		return fmt.Errorf("powerProvisioning.dps.requestTimeout must be greater than zero")
	}

	return nil
}

func (c *Config) validateDPSEnabled() (bool, error) {
	value := c.v.Get(ConfigDPSEnabled)
	switch typed := value.(type) {
	case nil:
		return false, nil
	case bool:
		return typed, nil
	case string:
		enabled, err := strconv.ParseBool(strings.TrimSpace(typed))
		if err == nil {
			return enabled, nil
		}
	}

	return false, fmt.Errorf("powerProvisioning.dps.enabled must be a boolean")
}

// NewRateLimiterConfig initializes and returns a configuration object for rate limiting
func NewRateLimiterConfig(enabled bool, rate float64, burst int, expiresIn int) *RateLimiterConfig {
	return &RateLimiterConfig{
		Enabled:   enabled,
		Rate:      rate,
		Burst:     burst,
		ExpiresIn: expiresIn,
	}
}

// GetIssuersConfig returns the issuer configurations from the config file
func (c *Config) GetIssuersConfig() ([]IssuerConfig, error) {
	var issuersConfig []IssuerConfig
	err := c.v.UnmarshalKey("issuers", &issuersConfig)
	if err != nil {
		return nil, fmt.Errorf("unmarshal issuers configuration: %w", err)
	}
	return issuersConfig, nil
}

// ValidateIssuersConfig validates the issuer configurations
func (c *Config) ValidateIssuersConfig(issuers []IssuerConfig) error {
	seenNames := make(map[string]bool)
	seenURLs := make(map[string]bool)
	seenStaticOrgs := make(map[string]bool)
	seenDynamicOrg := false
	originCounts := make(map[string]int)

	for i, issuer := range issuers {
		// Validate required fields
		if issuer.Name == "" {
			return fmt.Errorf("issuer %d: name is required", i)
		}

		if seenNames[issuer.Name] {
			return fmt.Errorf("duplicate issuer name: %s", issuer.Name)
		}
		seenNames[issuer.Name] = true

		origin, err := issuer.GetOrigin()
		if err != nil {
			return fmt.Errorf("issuer %s: %w", issuer.Name, err)
		}
		originCounts[origin]++

		if origin == cauth.TokenOriginKas {
			if err := issuer.ValidateKasOrigin(); err != nil {
				return err
			}
			continue
		}

		if issuer.JWKS == "" {
			return fmt.Errorf("issuer %s: jwks URL is required", issuer.Name)
		}

		if issuer.Issuer == "" {
			return fmt.Errorf("issuer %s: issuer is required", issuer.Name)
		}

		// Check for duplicate JWKS URLs
		if seenURLs[issuer.JWKS] {
			return fmt.Errorf("duplicate JWKS URL: %s (issuer: %s)", issuer.JWKS, issuer.Name)
		}
		seenURLs[issuer.JWKS] = true

		// ClaimMappings are only allowed for custom origin issuers
		// keycloak, kas-ssa, and kas-legacy have their own predefined claim extraction logic
		if len(issuer.ClaimMappings) > 0 && origin != cauth.TokenOriginCustom {
			return fmt.Errorf("issuer %s: claimMappings are only allowed for custom origin issuers (origin: %s)", issuer.Name, origin)
		}

		// Validate JWKS timeout if specified
		if issuer.JWKSTimeout != "" {
			if _, err := issuer.GetJWKSTimeout(); err != nil {
				return fmt.Errorf("issuer %s: invalid jwksTimeout: %w", issuer.Name, err)
			}
		}

		for j, mapping := range issuer.ClaimMappings {
			if mapping.OrgAttribute == "" && mapping.OrgName == "" {
				return fmt.Errorf("issuer %s: claimMapping %d: either orgAttribute or orgName must be specified", issuer.Name, j)
			}

			if mapping.OrgAttribute != "" && mapping.OrgName != "" {
				return fmt.Errorf("issuer %s: claimMapping %d: cannot specify both orgAttribute and orgName", issuer.Name, j)
			}

			// orgDisplayName can only be specified with orgName (static org), not with orgAttribute (dynamic org)
			if mapping.OrgDisplayName != "" && mapping.OrgName == "" {
				return fmt.Errorf("issuer %s: claimMapping %d: orgDisplayName can only be specified when orgName is specified", issuer.Name, j)
			}

			// roles and rolesAttribute are mutually exclusive
			if len(mapping.Roles) > 0 && mapping.RolesAttribute != "" {
				return fmt.Errorf("issuer %s: claimMapping %d: cannot specify both roles and rolesAttribute", issuer.Name, j)
			}

			// Service account validation
			if mapping.IsServiceAccount {
				if len(mapping.Roles) > 0 {
					return fmt.Errorf("issuer %s: claimMapping %d: roles cannot be specified when isServiceAccount is true", issuer.Name, j)
				}
				if mapping.RolesAttribute != "" {
					return fmt.Errorf("issuer %s: claimMapping %d: rolesAttribute cannot be specified when isServiceAccount is true", issuer.Name, j)
				}
				if mapping.OrgAttribute != "" {
					return fmt.Errorf("issuer %s: claimMapping %d: orgAttribute cannot be specified when isServiceAccount is true", issuer.Name, j)
				}
				if !c.GetEnvDisconnected() {
					return fmt.Errorf("issuer %s: claimMapping %d: isServiceAccount is only supported in disconnected mode", issuer.Name, j)
				}
			}

			// Dynamic org mapping
			if mapping.OrgAttribute != "" {
				if seenDynamicOrg {
					return fmt.Errorf("issuer %s: only one dynamic org mapping is allowed", issuer.Name)
				}
				seenDynamicOrg = true
			}

			// Static org mapping - check for duplicates unless allowDuplicateStaticOrgNames is true
			if mapping.OrgName != "" {
				normalizedOrg := strings.ToLower(mapping.OrgName)
				if seenStaticOrgs[normalizedOrg] && !issuer.GetAllowDuplicateStaticOrgNames() {
					return fmt.Errorf("issuer %s: duplicate static org: %s", issuer.Name, mapping.OrgName)
				}
				seenStaticOrgs[normalizedOrg] = true
			}

			// Validate roles - skip if service account (uses predefined roles)
			if !mapping.IsServiceAccount && len(mapping.Roles) == 0 && mapping.RolesAttribute == "" {
				return fmt.Errorf("issuer %s: claimMapping %d: either roles or rolesAttribute must be specified (unless isServiceAccount is true)", issuer.Name, j)
			}

			// Validate role values if specified
			for _, role := range mapping.Roles {
				if !cauth.IsValidRole(role) {
					return fmt.Errorf("issuer %s: claimMapping %d: invalid role: %s", issuer.Name, j, role)
				}
			}
		}

		// Service account validation
		if issuer.ServiceAccount {
			if !c.GetEnvDisconnected() {
				return fmt.Errorf("issuer %s: service account is only supported in disconnected mode", issuer.Name)
			}
		}
	}
	err := validateOriginCombination(originCounts)
	if err != nil {
		return err
	}

	return nil
}

// validateOriginCombination accepts one or more custom issuers on their own, or a
// kas or kas-ssa issuer optionally paired with kas-legacy. Keycloak is configured
// through its own keycloak block rather than the issuers array.
func validateOriginCombination(originCounts map[string]int) error {
	if originCounts[cauth.TokenOriginKeycloak] > 0 {
		return fmt.Errorf("origin: %s is configured through the keycloak settings, not the issuers list", cauth.TokenOriginKeycloak)
	}

	if originCounts[cauth.TokenOriginCustom] > 0 && len(originCounts) > 1 {
		return fmt.Errorf("origin: %s cannot be configured with any other origin", cauth.TokenOriginCustom)
	}

	if originCounts[cauth.TokenOriginKas] > 0 && originCounts[cauth.TokenOriginKasSsa] > 0 {
		return fmt.Errorf("origin: %s and %s cannot be configured together", cauth.TokenOriginKas, cauth.TokenOriginKasSsa)
	}

	// Each kas origin names a single upstream KAS deployment, so it cannot repeat.
	// Only custom, which covers arbitrary third-party issuers, may appear more than once.
	for _, origin := range []string{cauth.TokenOriginKas, cauth.TokenOriginKasLegacy, cauth.TokenOriginKasSsa} {
		if originCounts[origin] > 1 {
			return fmt.Errorf("only one issuer with origin: %s is allowed", origin)
		}
	}

	return nil
}

// ValidateKasOrigin validates a KAS issuer configuration. The issuer carries the
// NGC API base URL the processor calls, so it is required rather than defaulted.
func (ic *IssuerConfig) ValidateKasOrigin() error {
	if ic.JWKS != "" {
		return fmt.Errorf("issuer %s: jwks URL is not supported for origin: %s", ic.Name, cauth.TokenOriginKas)
	}

	baseURL, err := url.Parse(ic.Issuer)
	if err != nil || baseURL.Host == "" || baseURL.Scheme != "https" {
		return fmt.Errorf("issuer %s: issuer must be an absolute HTTPS NGC API URL for origin: %s", ic.Name, cauth.TokenOriginKas)
	}

	if baseURL.User != nil || baseURL.RawQuery != "" || baseURL.Fragment != "" {
		return fmt.Errorf("issuer %s: issuer must not contain user info, query, or fragment for origin: %s", ic.Name, cauth.TokenOriginKas)
	}

	if len(ic.ClaimMappings) > 0 {
		return fmt.Errorf("issuer %s: claimMappings are not supported for origin: %s", ic.Name, cauth.TokenOriginKas)
	}

	if ic.ServiceAccount {
		return fmt.Errorf("issuer %s: serviceAccount is not supported for origin: %s", ic.Name, cauth.TokenOriginKas)
	}

	return nil
}

/* Config getters */

// GetPathToConfig returns the path to the config file
func (c *Config) GetPathToConfig() string {
	return c.v.GetString(ConfigFilePath)
}

// GetEnvDev returns if the service is running in development mode
func (c *Config) GetEnvDev() bool {
	s := c.v.GetBool(ConfigEnvDev)
	return s
}

// GetEnvDisconnected returns if the service is running in disconnected mode
func (c *Config) GetEnvDisconnected() bool {
	return c.v.GetBool(ConfigEnvDisconnected)
}

// GetAPIName returns the name of the API
func (c *Config) GetAPIName() string {
	return c.v.GetString(ConfigAPIName)
}

// GetAPIRouteVersion returns the version of the API
func (c *Config) GetAPIRouteVersion() string {
	return c.v.GetString(ConfigAPIRouteVersion)
}

// GetLogLevel returns the log level
func (c *Config) GetLogLevel() string {
	return c.v.GetString(ConfigLogLevel)
}

// GetSentryDSN returns the DSN for Sentry
func (c *Config) GetSentryDSN() string {
	return c.v.GetString(ConfigSentryDSN)
}

// GetDPSEnabled reports whether NICo makes direct DPS calls.
func (c *Config) GetDPSEnabled() bool {
	return c.v.GetBool(ConfigDPSEnabled)
}

// GetDBHost returns the host of the database
func (c *Config) GetDBHost() string {
	return c.v.GetString(ConfigDBHost)
}

// GetDBPort returns the port of the database
func (c *Config) GetDBPort() int {
	return c.v.GetInt(ConfigDBPort)
}

// GetDBName returns the name of the database
func (c *Config) GetDBName() string {
	return c.v.GetString(ConfigDBName)
}

// GetDBUser returns the user of the database
func (c *Config) GetDBUser() string {
	return c.v.GetString(ConfigDBUser)
}

// GetDBPasswordPath returns the path for file containing password of the database
func (c *Config) GetDBPasswordPath() string {
	return c.v.GetString(ConfigDBPasswordPath)
}

// GetDBPassword returns the password of the database
func (c *Config) GetDBPassword() string {
	c.RLock()
	defer c.RUnlock()
	return c.v.GetString(ConfigDBPassword)
}

// GetTemporalHost returns the hostname for Temporal
func (c *Config) GetTemporalHost() string {
	return c.v.GetString(ConfigTemporalHost)
}

// GetTemporalPort returns the port for Temporal
func (c *Config) GetTemporalPort() int {
	return c.v.GetInt(ConfigTemporalPort)
}

// GetTemporalServerName returns the server name for Temporal
func (c *Config) GetTemporalServerName() string {
	return c.v.GetString(ConfigTemporalServerName)
}

// GetTemporalNamespace returns the namespace for Temporal
func (c *Config) GetTemporalNamespace() string {
	return c.v.GetString(ConfigTemporalNamespace)
}

// GetTemporalQueue returns the queue for Temporal
func (c *Config) GetTemporalQueue() string {
	return c.v.GetString(ConfigTemporalQueue)
}

// GetTemporalTlsEnabled returns true if TLS is enabled for Temporal connection
func (c *Config) GetTemporalTlsEnabled() bool {
	return c.v.GetBool(ConfigTemporalTlsEnabled)
}

// GetTemporalCertPath returns the path for client cert for Temporal
func (c *Config) GetTemporalCertPath() string {
	return c.v.GetString(ConfigTemporalCertPath)
}

// SetTemporalCertPath set the path for client cert for Temporal
func (c *Config) SetTemporalCertPath(value string) {
	c.v.Set(ConfigTemporalCertPath, value)
}

// GetTemporalKeyPath returns the path for client key for Temporal
func (c *Config) GetTemporalKeyPath() string {
	return c.v.GetString(ConfigTemporalKeyPath)
}

// SetTemporalKeyPath set the path for client key for Temporal
func (c *Config) SetTemporalKeyPath(value string) {
	c.v.Set(ConfigTemporalKeyPath, value)
}

// GetTemporalCaPath returns the path for ca cert for Temporal
func (c *Config) GetTemporalCaPath() string {
	return c.v.GetString(ConfigTemporalCaPath)
}

// SetTemporalCaPath sets the path for ca for Temporal
func (c *Config) SetTemporalCaPath(value string) {
	c.v.Set(ConfigTemporalCaPath, value)
}

// GetTemporalEncryptionKeyPath returns the path for file containing encryption key for Temporal
func (c *Config) GetTemporalEncryptionKeyPath() string {
	return c.v.GetString(ConfigTemporalEncryptionKeyPath)
}

// SetTemporalEncryptionKeyPath sets the path for file containing encryption key for Temporal
func (c *Config) SetTemporalEncryptionKeyPath(value string) {
	c.v.Set(ConfigTemporalEncryptionKeyPath, value)
}

// GetTemporalEncryptionKey returns the encryption key for Temporal
func (c *Config) GetTemporalEncryptionKey() string {
	return c.v.GetString(ConfigTemporalEncryptionKey)
}

// SetTemporalEncryptionKey sets the encryption key for Temporal
func (c *Config) SetTemporalEncryptionKey() {
	log.Warn().Str("temporal.encryptionKeyPath", c.GetTemporalEncryptionKeyPath()).Msg("setting Temporal encryption key by reading from file")

	encryptionKeyBytes, err := os.ReadFile(c.GetTemporalEncryptionKeyPath())
	if err != nil {
		log.Panic().Err(err).Msgf("failed to read encryption key file: %s", err)
	}
	encryptionKey := strings.TrimSpace(string(encryptionKeyBytes))

	c.v.Set(ConfigTemporalEncryptionKey, encryptionKey)
}

// ValidateSiteConfig validates Site configs
func (c *Config) ValidateSiteConfig() error {
	if c.GetSitePhoneHomeUrl() == "" {
		return fmt.Errorf("invalid Site PhoneHome url")
	}
	return nil
}

// SetSiteManagerEnabled sets the enabled field
func (c *Config) SetSiteManagerEnabled(value bool) {
	c.v.Set(ConfigSiteManagerEnabled, value)
}

// GetSiteManagerEnabled sets the enabled field
func (c *Config) GetSiteManagerEnabled() bool {
	return c.v.GetBool(ConfigSiteManagerEnabled)
}

// SetSiteManagerEndpoint sets the endpoint
func (c *Config) SetSiteManagerEndpoint(value string) {
	c.v.Set(ConfigSiteManagerEndpoint, value)
}

// GetSiteManagerEndpoint gets the path for site manager service endpoint
func (c *Config) GetSiteManagerEndpoint() string {
	return c.v.GetString(ConfigSiteManagerEndpoint)
}

// SetSitePhoneHomeUrl sets the url for PhoneHome
func (c *Config) SetSitePhoneHomeUrl(value string) {
	// Lock the mutex to prevent race conditions
	c.Lock()
	defer c.Unlock()
	c.v.Set(ConfigSitePhoneHomeUrl, value)
}

// GetSitePhoneHomeUrl gets the url for PhoneHome
func (c *Config) GetSitePhoneHomeUrl() string {
	// Lock the mutex to prevent race conditions
	c.RLock()
	defer c.RUnlock()
	return c.v.GetString(ConfigSitePhoneHomeUrl)
}

// GetMetricsEnabled gets the enabled field for Metrics
func (c *Config) GetMetricsEnabled() bool {
	return c.v.GetBool(ConfigMetricsEnabled)
}

// GetZincSearchPort gets the port for Metrics
func (c *Config) GetMetricsPort() int {
	return c.v.GetInt(ConfigMetricsPort)
}

// GetMetricsNamespace gets the prefix applied to every exposed metric name.
// An explicitly empty value falls back to the default, since echoprometheus
// substitutes its own "echo" prefix for an empty one.
func (c *Config) GetMetricsNamespace() string {
	namespace := c.v.GetString(ConfigMetricsNamespace)
	if namespace == "" {
		return DefaultMetricsNamespace
	}
	return namespace
}

// GetTracingEnabled gets the enabled field for tracing
func (c *Config) GetTracingEnabled() bool {
	return c.v.GetBool(ConfigTracingEnabled)
}

// GetTracingServiceName gets the service name for tracing
func (c *Config) GetTracingServiceName() string {
	return c.v.GetString(ConfigTracingServiceName)
}

// Keycloak configuration methods

// GetKeycloakEnabled gets the enabled field for Keycloak
func (c *Config) GetKeycloakEnabled() bool {
	return c.v.GetBool(ConfigKeycloakEnabled)
}

// SetKeycloakEnabled sets the enabled field for Keycloak
func (c *Config) SetKeycloakEnabled(value bool) {
	c.v.Set(ConfigKeycloakEnabled, value)
}

// GetKeycloakBaseURL gets the base URL for Keycloak
func (c *Config) GetKeycloakBaseURL() string {
	return c.v.GetString(ConfigKeycloakBaseURL)
}

// SetKeycloakBaseURL sets the base URL for Keycloak
func (c *Config) SetKeycloakBaseURL(value string) {
	c.v.Set(ConfigKeycloakBaseURL, value)
}

// GetKeycloakExternalBaseURL gets the external base URL for Keycloak
func (c *Config) GetKeycloakExternalBaseURL() string {
	return c.v.GetString(ConfigKeycloakExternalBaseURL)
}

// SetKeycloakExternalBaseURL sets the external base URL for Keycloak
func (c *Config) SetKeycloakExternalBaseURL(value string) {
	c.v.Set(ConfigKeycloakExternalBaseURL, value)
}

// GetKeycloakRealm gets the realm for Keycloak
func (c *Config) GetKeycloakRealm() string {
	return c.v.GetString(ConfigKeycloakRealm)
}

// SetKeycloakRealm sets the realm for Keycloak
func (c *Config) SetKeycloakRealm(value string) {
	c.v.Set(ConfigKeycloakRealm, value)
}

// GetKeycloakClientID gets the client ID for Keycloak
func (c *Config) GetKeycloakClientID() string {
	return c.v.GetString(ConfigKeycloakClientID)
}

// SetKeycloakClientID sets the client ID for Keycloak
func (c *Config) SetKeycloakClientID(value string) {
	c.v.Set(ConfigKeycloakClientID, value)
}

// GetKeycloakClientSecretPath gets the client secret path for Keycloak
func (c *Config) GetKeycloakClientSecretPath() string {
	return c.v.GetString(ConfigKeycloakClientSecretPath)
}

// SetKeycloakClientSecretPath sets the client secret path for Keycloak
func (c *Config) SetKeycloakClientSecretPath(value string) {
	c.v.Set(ConfigKeycloakClientSecretPath, value)
}

// GetKeycloakClientSecret gets the client secret for Keycloak
func (c *Config) GetKeycloakClientSecret() string {
	return c.v.GetString(ConfigKeycloakClientSecret)
}

// SetKeycloakClientSecret sets the client secret for Keycloak
func (c *Config) SetKeycloakClientSecret() {
	log.Warn().Str("keycloak.clientSecretPath", c.GetKeycloakClientSecretPath()).Msg("setting keycloak client secret by reading from secret path")

	secretBytes, err := os.ReadFile(c.GetKeycloakClientSecretPath())
	if err != nil {
		log.Panic().Err(err).Msgf("failed to read keycloak client secret file: %s", err)
	}
	secret := strings.TrimSpace(string(secretBytes))

	c.v.Set(ConfigKeycloakClientSecret, secret)
}

// GetKeycloakServiceAccountEnabled gets the service account enabled flag for Keycloak
func (c *Config) GetKeycloakServiceAccountEnabled() bool {
	return c.v.GetBool(ConfigKeycloakServiceAccountEnabled)
}

// SetKeycloakServiceAccountEnabled sets the service account enabled flag for Keycloak
func (c *Config) SetKeycloakServiceAccountEnabled(value bool) {
	c.v.Set(ConfigKeycloakServiceAccountEnabled, value)
}

// GetOrInitKeycloakConfig gets the Keycloak configuration
func (c *Config) GetOrInitKeycloakConfig() (*cauth.KeycloakConfig, error) {
	if c.KeycloakConfig == nil {
		if !c.GetKeycloakEnabled() {
			log.Info().Msg("Keycloak is disabled in configuration")
			return nil, errors.New("Keycloak is not enabled")
		}

		log.Info().Str("baseURL", c.GetKeycloakBaseURL()).
			Str("externalBaseURL", c.GetKeycloakExternalBaseURL()).
			Str("realm", c.GetKeycloakRealm()).
			Str("clientID", c.GetKeycloakClientID()).
			Str("clientSecretPath", c.GetKeycloakClientSecretPath()).
			Msg("Creating new Keycloak configuration")

		c.KeycloakConfig = cauth.NewKeycloakConfig(
			c.GetKeycloakBaseURL(),
			c.GetKeycloakExternalBaseURL(),
			c.GetKeycloakClientID(),
			c.GetKeycloakClientSecret(),
			c.GetKeycloakRealm(),
			c.GetKeycloakServiceAccountEnabled(),
		)
		log.Info().Msg("Keycloak configuration created successfully")
	}
	return c.KeycloakConfig, nil
}

// ValidateKeycloakConfig validates the Keycloak configuration
func (c *Config) ValidateKeycloakConfig() error {
	if !c.GetKeycloakEnabled() {
		return nil // Skip validation if Keycloak is not enabled
	}

	if c.GetKeycloakBaseURL() == "" {
		return fmt.Errorf("keycloak base URL must be specified when Keycloak is enabled")
	}

	if c.GetKeycloakRealm() == "" {
		return fmt.Errorf("keycloak realm must be specified when Keycloak is enabled")
	}

	if c.GetKeycloakClientID() == "" {
		return fmt.Errorf("keycloak client ID must be specified when Keycloak is enabled")
	}

	if c.GetKeycloakClientSecret() == "" && c.GetKeycloakClientSecretPath() == "" {
		return fmt.Errorf("keycloak client secret or client secret path must be specified when Keycloak is enabled")
	}

	return nil
}

// WatchSecretFilePaths starts watching secret files for changes.
func (c *Config) WatchSecretFilePaths() {
	log.Info().Str("WatchSecretFilePaths", "").Msg("started watching secret file paths")
	initWG := sync.WaitGroup{}
	initWG.Add(1)
	go func() {
		watcher, err := fsnotify.NewWatcher()
		if err != nil {
			log.Panic().Err(err).Msgf("fatal error while creating watcher for secret files: %s", err)
		}
		defer watcher.Close()

		// Add new secret file and path here

		// db password secret file and path
		dbPassFilePath := filepath.Clean(c.GetDBPasswordPath())
		dbPassFileDir := ""
		if dbPassFilePath != "" {
			dbPassFileDir, _ = filepath.Split(dbPassFilePath)
		}

		// keycloak client secret file and path
		keycloakClientSecretFilePath := filepath.Clean(c.GetKeycloakClientSecretPath())
		keycloakClientSecretFileDir := ""
		if keycloakClientSecretFilePath != "" {
			keycloakClientSecretFileDir, _ = filepath.Split(keycloakClientSecretFilePath)
		}

		eventsWG := sync.WaitGroup{}
		eventsWG.Add(1)
		go func() {
			for {
				select {
				case event, ok := <-watcher.Events:
					if !ok { // 'Events' channel is closed
						eventsWG.Done()
						return
					}
					if !(event.Has(fsnotify.Write) || event.Has(fsnotify.Create)) {
						continue
					}
					eventFilePath := filepath.Clean(event.Name)
					if eventFilePath == "" {
						continue
					}
					eventFileDir, _ := filepath.Split(eventFilePath)
					if eventFileDir == dbPassFileDir {
						c.setDBPassword()
					}
					if eventFileDir == keycloakClientSecretFileDir && c.GetKeycloakEnabled() && keycloakClientSecretFileDir != "" {
						c.SetKeycloakClientSecret()
					}
				case err, ok := <-watcher.Errors:
					if ok { // 'Errors' channel is not closed
						log.Info().Err(err).Msgf("one or more secret file received watcher error: %s", err)
					}
					eventsWG.Done()
					return
				}
			}
		}()

		// Add secret file paths to watcher
		log.Info().Str("WatchSecretFilePaths", dbPassFileDir).Msg("adding db path for watcher")
		watcher.Add(dbPassFileDir)

		if c.GetKeycloakEnabled() {
			log.Info().Str("WatchSecretFilePaths", keycloakClientSecretFileDir).Msg("adding keycloak client secret path for watcher")
			watcher.Add(keycloakClientSecretFileDir)
		}

		initWG.Done()   // done initializing the watch in this go routine, so the parent routine can move on...
		eventsWG.Wait() // now, wait for event loop to end in this go-routine...
	}()
	initWG.Wait() // make sure that the go routine above fully ended before returning
}

// WatchConfigFile starts watching the config file for reloadable setting changes.
func (c *Config) WatchConfigFile() {
	configPath := c.GetPathToConfig()
	if absPath, err := filepath.Abs(configPath); err == nil {
		configPath = absPath
	}

	watchv := newViper()
	watchv.SetConfigFile(configPath)
	watchv.OnConfigChange(func(e fsnotify.Event) {
		if !e.Has(fsnotify.Write) {
			return
		}

		log.Info().Str("config.file", configPath).Str("event.file", e.Name).Msg("config file changed")

		// Dynamically reload config changes here
		// NOTE: Each attribute we decide to reload must support read/write locking in get/set methods
		// 1. Site PhoneHome URL
		newSitePhoneHomeUrl := watchv.GetString(ConfigSitePhoneHomeUrl)
		currentSitePhoneHomeUrl := c.GetSitePhoneHomeUrl()
		if newSitePhoneHomeUrl != "" && newSitePhoneHomeUrl != currentSitePhoneHomeUrl {
			c.SetSitePhoneHomeUrl(newSitePhoneHomeUrl)
			log.Info().Str("config.file", configPath).Str("event.file", e.Name).Msg("Successfully loaded new Site phone home URL")
		}
	})
	watchv.WatchConfig()
}

// Close stops background tasks
func (c *Config) Close() {
	if c.temporal != nil {
		c.temporal.Close()
	}
}

// Rate limiter configuration methods

// GetRateLimiterEnabled gets the enabled field for rate limiter
func (c *Config) GetRateLimiterEnabled() bool {
	return c.v.GetBool(ConfigRateLimiterEnabled)
}

// SetRateLimiterEnabled sets the enabled field for rate limiter
func (c *Config) SetRateLimiterEnabled(value bool) {
	c.v.Set(ConfigRateLimiterEnabled, value)
}

// GetRateLimiterRate gets the rate limit (requests per second)
func (c *Config) GetRateLimiterRate() float64 {
	return c.v.GetFloat64(ConfigRateLimiterRate)
}

// SetRateLimiterRate sets the rate limit (requests per second)
func (c *Config) SetRateLimiterRate(value float64) {
	c.v.Set(ConfigRateLimiterRate, value)
}

// GetRateLimiterBurst gets the burst size
func (c *Config) GetRateLimiterBurst() int {
	return c.v.GetInt(ConfigRateLimiterBurst)
}

// SetRateLimiterBurst sets the burst size
func (c *Config) SetRateLimiterBurst(value int) {
	c.v.Set(ConfigRateLimiterBurst, value)
}

// GetRateLimiterExpiresIn gets the expiration time in seconds
func (c *Config) GetRateLimiterExpiresIn() int {
	return c.v.GetInt(ConfigRateLimiterExpiresIn)
}

// SetRateLimiterExpiresIn sets the expiration time in seconds
func (c *Config) SetRateLimiterExpiresIn(value int) {
	c.v.Set(ConfigRateLimiterExpiresIn, value)
}

func newViper() *viper.Viper {
	return viper.NewWithOptions(viper.WithLogger(slog.New(zerologSlogHandler{
		logger: log.Logger.With().Str("component", "viper").Logger(),
	})))
}
