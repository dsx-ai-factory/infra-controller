// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	"github.com/fsnotify/fsnotify"
	"github.com/spf13/viper"

	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
)

const (
	// ConfigFilePath specifies the path to the config file, this contains the default path
	ConfigFilePath = "config.file"
	// ConfigFilePathEnv specifies the environment variable name for the config file path
	ConfigFilePathEnv = "CONFIG_FILE_PATH"

	// ConfigDevMode specifies if the service is running in dev mode
	ConfigDevMode = "env.dev"
	// ConfigLogLevel specifies the log level
	ConfigLogLevel = "log.level"

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
	// ConfigTemporalNamespaceEnv specifies the env var to read namespace for Temporal
	ConfigTemporalNamespaceEnv = "TEMPORAL_NAMESPACE"

	// ConfigTemporalQueue specifies the queue for Temporal
	ConfigTemporalQueue = "temporal.queue"
	// ConfigTemporalQueueEnv specifies the env var to read queue for Temporal
	ConfigTemporalQueueEnv = "TEMPORAL_QUEUE"

	// ConfigTemporalEncryptionKey specifies the data encryption key for Temporal
	ConfigTemporalEncryptionKey = "temporal.encryptionKey"
	// ConfigTemporalEncryptionKeyPath specifies the path for file containing data encryption key for Temporal
	ConfigTemporalEncryptionKeyPath = "temporal.encryptionKeyPath"

	// ConfigTemporalTlsEnabled specifies if TLS should be used with Temporal
	ConfigTemporalTlsEnabled = "temporal.tls.enabled"
	// ConfigTemporalCertPath specifies the path for client cert
	ConfigTemporalCertPath = "temporal.tls.certPath"
	// ConfigTemporalKeyPath specifies the path for client key
	ConfigTemporalKeyPath = "temporal.tls.keyPath"
	// ConfigTemporalCaPath specifies the path for server CA cert
	ConfigTemporalCaPath = "temporal.tls.caPath"

	// ConfigNgcAPIBaseURL specifies the base URL for the NGC API
	ConfigNgcAPIBaseURL = "ngc.api.baseUrl"

	// ConfigNotificationsSlackWebhookURL specifies the Slack webhook URL
	ConfigNotificationsSlackWebhookURL = "notifications.slack.webhookURL"
	// ConfigNotificationsSlackWebhookURLPath specifies file path to read Slack webhook URL
	ConfigNotificationsSlackWebhookURLPath = "notifications.slack.webhookURLPath"

	// ConfigSiteManagerEndpoint is the service endpoint for site manager
	ConfigSiteManagerEndpoint = "siteManager.svcEndpoint"

	// ConfigMetricsEnabled is a feature flag for Prometheus metrics
	ConfigMetricsEnabled = "metrics.enabled"
	// ConfigMetricsPort specifies the port for Prometheus metrics
	ConfigMetricsPort = "metrics.port"
	// ConfigMetricsNamespace specifies the prefix for every exposed metric name
	ConfigMetricsNamespace = "metrics.namespace"

	// ConfigHealthzEnabled is a feature flag for health check endpoint
	ConfigHealthzEnabled = "healthz.enabled"
	// ConfigHealthzPort specifies the port for health check endpoint
	ConfigHealthzPort = "healthz.port"

	// ConfigTracingEnabled is a feature flag for tracing
	ConfigTracingEnabled = "tracing.enabled"
	// ConfigTracingServiceName specifies the service name for tracing
	ConfigTracingServiceName = "tracing.serviceName"

	// ConfigWorkerMaxConcurrentActivityPollers specifies how many concurrent activity
	// task pollers the Temporal worker runs. This service reads only from config.yaml
	// (Helm-rendered) -- unlike Site Agent's old-style config, there's no env var override.
	ConfigWorkerMaxConcurrentActivityPollers = "worker.maxConcurrentActivityPollers"
)

const (
	// DefaultMaxConcurrentActivityPollers is the poller count used when config.yaml
	// doesn't set worker.maxConcurrentActivityPollers. Matches the historical hardcoded value.
	DefaultMaxConcurrentActivityPollers = 10
	// MaxMaxConcurrentActivityPollers caps the configurable poller count. Each poller can
	// hold a DB connection from the shared pgx pool while its activity runs, and this worker
	// also serves the cloud task queue. Lowered from an initial 200 (an unguessed
	// fat-finger-only bound) to 20 per review: benchmarking found no throughput gain going
	// from poller=10 to poller=40 at page_size=50, and Temporal's own worker design means a
	// handful of pollers already saturates typical activity-execution-slot counts -- raising
	// this further is unlikely to help and mainly risks pgx pool exhaustion on a worker that
	// also serves the cloud task queue. Bump with fresh benchmark data if a real need appears.
	MaxMaxConcurrentActivityPollers = 20
)

// Maintain a global config object
var config *Config

// Config represents configurations for the service
type Config struct {
	v        *viper.Viper
	db       *cconfig.DBConfig
	temporal *cconfig.TemporalConfig
}

// NewConfig creates a new config object
func NewConfig() *Config {
	if config != nil {
		return config
	}

	c := Config{
		v: viper.New(),
	}

	// Set defaults
	c.v.SetDefault(ConfigLogLevel, "info")
	c.v.SetDefault(ConfigDevMode, false)

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

	c.v.SetDefault(ConfigHealthzEnabled, true)
	c.v.SetDefault(ConfigHealthzPort, 8899)

	c.v.SetDefault(ConfigTracingEnabled, false)

	c.v.SetDefault(ConfigWorkerMaxConcurrentActivityPollers, DefaultMaxConcurrentActivityPollers)

	c.v.AutomaticEnv()
	c.v.SetConfigFile(c.GetPathToConfig())

	log.Warn().Str("path", c.GetPathToConfig()).Msg("loading config")

	err := c.v.ReadInConfig() // Find and read the config file
	if _, ok := err.(*os.PathError); ok {
		log.Warn().Msgf("no config file '%s' not found. Using default values", c.GetPathToConfig())
	} else if err != nil { // Handle other errors that occurred while reading the config file
		log.Panic().Err(err).Msgf("fatal error while reading the config file: %s", err)
	}

	// Set values
	c.setLogLevel()

	if c.GetDBPasswordPath() != "" {
		c.setDBPassword()
	}

	if c.GetNotificationsSlackWebhookURLPath() != "" {
		c.setNotificationsSlackWebhookURL()
	}

	c.setTemporalNamespace()
	c.setTemporalQueue()

	if c.GetTemporalEncryptionKey() == "" {
		if c.GetTemporalEncryptionKeyPath() != "" {
			c.SetTemporalEncryptionKey()
		} else {
			log.Panic().Msg("temporal encryption key or encryption key path config must be specified")
		}
	}

	// Validate config
	c.Validate()

	// Watch secret files
	c.WatchSecretFilePaths()

	config = &c

	return config
}

// Validate validates the config
func (c *Config) Validate() {
	// Check configs that are essential but were not set
	if c.GetDBName() == "" {
		log.Panic().Msg("db name config must be specified")
	}

	if c.GetDBUser() == "" {
		log.Panic().Msg("db user config must be specified")
	}

	if c.GetDBPassword() == "" && c.GetDBPasswordPath() == "" {
		log.Panic().Msg("db password or password path config must be specified")
	}

	if c.GetTemporalTlsEnabled() {
		if c.GetTemporalCertPath() == "" {
			log.Panic().Msg("temporal cert path config must be specified")
		}

		if c.GetTemporalKeyPath() == "" {
			log.Panic().Msg("temporal key path config must be specified")
		}

		if c.GetTemporalCaPath() == "" {
			log.Panic().Msg("temporal ca path config must be specified")
		}
	}

	if c.GetTemporalServerName() == "" {
		log.Panic().Msg("temporal server name config must be specified")
	}

	if c.GetTemporalNamespace() == "" {
		log.Panic().Msg("temporal namespace config must be specified")
	}

	if c.GetTemporalQueue() == "" {
		log.Panic().Msg("temporal queue config must be specified")
	}

	if c.GetTemporalEncryptionKey() == "" {
		log.Error().Msg("temporal encryption key config was not specified, arguments won't be encrypted")
	}

	if c.GetNgcAPIBaseURL() == "" {
		log.Warn().Msg("ngc api base url config not specified, NGC user lookups will be unavailable")
	}

	if p := c.GetMaxConcurrentActivityPollers(); p < 1 || p > MaxMaxConcurrentActivityPollers {
		log.Panic().Msgf("worker max concurrent activity pollers %d must be between 1 and %d", p, MaxMaxConcurrentActivityPollers)
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
	log.Warn().Str("db.passwordPath", c.GetDBPasswordPath()).Msg("setting DB password by reading from password path")

	passwordBytes, err := os.ReadFile(c.GetDBPasswordPath())
	if err != nil {
		log.Err(err).Str("db.passwordPath", c.GetDBPasswordPath()).Msg("failed to read DB password from file")
	} else {
		c.v.Set(ConfigDBPassword, string(passwordBytes))
	}
}

// setNotificationsSlackWebhookURL sets the Slack webhook URL by reading from webhook URL path
func (c *Config) setNotificationsSlackWebhookURL() {
	log.Warn().Str("notifications.slack.webhookURLPath", c.GetNotificationsSlackWebhookURLPath()).Msg("setting Slack webhook URL by reading from webhook URL path")

	webhookURLBytes, err := os.ReadFile(c.GetNotificationsSlackWebhookURLPath())
	if err != nil {
		log.Err(err).Str("notifications.slack.webhookURLPath", c.GetNotificationsSlackWebhookURLPath()).Msg("failed to read Slack webhook URL from file")
	} else {
		c.v.Set(ConfigNotificationsSlackWebhookURL, string(webhookURLBytes))
	}
}

// setTemporalNamespace sets the namespace for the temporal client
func (c *Config) setTemporalNamespace() {
	// Check for env var override
	tn := os.Getenv(ConfigTemporalNamespaceEnv)
	if tn != "" {
		c.v.Set(ConfigTemporalNamespace, tn)
	}
}

// setTemporalQueue sets the queue for the temporal client
func (c *Config) setTemporalQueue() {
	// Check env var
	tq := os.Getenv(ConfigTemporalQueueEnv)
	if tq != "" {
		c.v.Set(ConfigTemporalQueue, tq)
	}
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

// GetMetricsConfig returns the Metrics config
func (c *Config) GetMetricsConfig() *MetricsConfig {
	return NewMetricsConfig(c.GetMetricsEnabled(), c.GetMetricsPort(), c.GetMetricsNamespace())
}

// GetHealthzConfig returns the Healthz config
func (c *Config) GetHealthzConfig() *HealthzConfig {
	return NewHealthzConfig(c.GetHealthzEnabled(), c.GetHealthzPort())
}

/* Config getters */

// GetLogLevel returns the log level
func (c *Config) GetLogLevel() string {
	s := c.v.GetString(ConfigLogLevel)
	return s
}

// GetPathToConfig returns the path to the config file
func (c *Config) GetPathToConfig() string {
	return c.v.GetString(ConfigFilePath)
}

// GetDevMode returns if the service is running in dev mode
func (c *Config) GetDevMode() bool {
	s := c.v.GetBool(ConfigDevMode)
	return s
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
	return c.v.GetString(ConfigDBPassword)
}

// GetMaxConcurrentActivityPollers returns the number of concurrent activity task pollers
// the Temporal worker should run
func (c *Config) GetMaxConcurrentActivityPollers() int {
	return c.v.GetInt(ConfigWorkerMaxConcurrentActivityPollers)
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

// GetTemporalEncryptionKeyPath returns the path for file containing data encryption key for Temporal
func (c *Config) GetTemporalEncryptionKeyPath() string {
	return c.v.GetString(ConfigTemporalEncryptionKeyPath)
}

// SetTemporalEncryptionKeyPath sets the path for file containing data encryption key for Temporal
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

// GetNgcAPIBaseURL returns the base url for the ngc api
func (c *Config) GetNgcAPIBaseURL() string {
	return c.v.GetString(ConfigNgcAPIBaseURL)
}

// SetNgcAPIBaseURL sets the base url for the ngc api
func (c *Config) SetNgcAPIBaseURL(value string) {
	c.v.Set(ConfigNgcAPIBaseURL, value)
}

// GetNotificationsSlackEnabled returns if Slack notifications are enabled
func (c *Config) GetNotificationsSlackEnabled() bool {
	return c.GetNotificationsSlackWebhookURL() != "" || c.GetNotificationsSlackWebhookURLPath() != ""
}

// GetNotificationsSlackWebhookURL gets the slack webhook url
func (c *Config) GetNotificationsSlackWebhookURL() string {
	return c.v.GetString(ConfigNotificationsSlackWebhookURL)
}

// SetNotificationsSlackWebhookURL sets the slack webhook url
func (c *Config) SetNotificationsSlackWebhookURL(value string) {
	c.v.Set(ConfigNotificationsSlackWebhookURL, value)
}

// GetNotificationsSlackWebhookURLPath gets the file path to read slack webhook url
func (c *Config) GetNotificationsSlackWebhookURLPath() string {
	return c.v.GetString(ConfigNotificationsSlackWebhookURLPath)
}

// SetSiteManagerEndpoint sets the endpoint
func (c *Config) SetSiteManagerEndpoint(value string) {
	c.v.Set(ConfigSiteManagerEndpoint, value)
}

// GetSiteManagerEndpoint gets the path for site manager service endpoint
func (c *Config) GetSiteManagerEndpoint() string {
	return c.v.GetString(ConfigSiteManagerEndpoint)
}

// GetMetricsEnabled gets the enabled field for Metrics
func (c *Config) GetMetricsEnabled() bool {
	return c.v.GetBool(ConfigMetricsEnabled)
}

// GetMetricsPort gets the port for Metrics
func (c *Config) GetMetricsPort() int {
	return c.v.GetInt(ConfigMetricsPort)
}

// GetMetricsNamespace gets the prefix applied to every exposed metric name.
// An explicitly empty value falls back to the default rather than exposing
// unprefixed names that would collide with another service's.
func (c *Config) GetMetricsNamespace() string {
	namespace := c.v.GetString(ConfigMetricsNamespace)
	if namespace == "" {
		return DefaultMetricsNamespace
	}
	return namespace
}

// GetHealthzEnabled gets the enabled field for Healthz
func (c *Config) GetHealthzEnabled() bool {
	return c.v.GetBool(ConfigHealthzEnabled)
}

// GetHealthzPort gets the port for Healthz
func (c *Config) GetHealthzPort() int {
	return c.v.GetInt(ConfigHealthzPort)
}

// GetTracingEnabled gets the enabled field for tracing
func (c *Config) GetTracingEnabled() bool {
	return c.v.GetBool(ConfigTracingEnabled)
}

// GetTracingServiceName gets the service name for tracing
func (c *Config) GetTracingServiceName() string {
	return c.v.GetString(ConfigTracingServiceName)
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

		initWG.Done()   // done initializing the watch in this go routine, so the parent routine can move on...
		eventsWG.Wait() // now, wait for event loop to end in this go-routine...
	}()
	initWG.Wait() // make sure that the go routine above fully ended before returning
}

// Close stops background tasks
func (c *Config) Close() {
	if c.temporal != nil {
		c.temporal.Close()
	}
}
