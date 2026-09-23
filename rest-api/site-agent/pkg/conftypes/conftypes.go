// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package conftypes

import (
	"encoding/json"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

// RunInEnvironment provides a strongly-typed indicator for the environment
// in which the app is running.
type RunInEnvironment int

// DefaultMetricsNamespace prefixes every metric the Site Agent exposes and
// matches its nico-rest-site-agent Helm service name. Operators override it with
// METRICS_NAMESPACE, which the config manager resolves once so the six manager
// packages that declare metrics all read the same value.
const DefaultMetricsNamespace = "nico_rest_site_agent"

const (
	// RunningInUnknown - Running In Unknown Env
	RunningInUnknown RunInEnvironment = iota
	// RunningInDocker - Running In Docker
	RunningInDocker
	// RunningInK8s - Running In K8s
	RunningInK8s
)

// TemporalConfig holds configurations for connecting to Temporal server
type TemporalConfig struct {
	Host                       string `json:"host"`
	Port                       string `json:"port"`
	ClusterID                  string `json:"clusterID"`
	TemporalServer             string `json:"temporalServer"`
	TemporalPublishNamespace   string `json:"temporalPublishNamespace"`
	TemporalSubscribeNamespace string `json:"temporalSubscribeNamespace"`
	TemporalPublishQueue       string `json:"temporalPublishQueue"`
	TemporalSubscribeQueue     string `json:"temporalSubscribeQueue"`
	TemporalInventorySchedule  string `json:"temporalInventorySchedule"`
	TemporalCertPath           string `json:"temporalCertPath"`
	// InventoryCloudPageSize is the number of inventory items published to Cloud per
	// Temporal workflow page. Read from INVENTORY_CLOUD_PAGE_SIZE, defaulting to
	// DefaultInventoryCloudPageSize; bounded by MaxInventoryCloudPageSize.
	InventoryCloudPageSize int `json:"inventoryCloudPageSize"`
}

const (
	// DefaultInventoryCloudPageSize is the fallback page size when
	// INVENTORY_CLOUD_PAGE_SIZE is unset. Matches the historical hardcoded value.
	DefaultInventoryCloudPageSize = 25
	// MaxInventoryCloudPageSize caps the configurable page size. Do not read it as a size that
	// fits Temporal's 2MB blob limit: a real Machine has measured at 49KB after pruning and
	// 156KB before it, so 100 of them exceeds the limit either way. Staying under it is the
	// publish ladder's job, which shrinks a page until it measures small enough, and this bound
	// only keeps the starting page size somewhere sensible.
	MaxInventoryCloudPageSize = 100
)

// GetTemporalCertOTPFullPath - Get Temporal Cert OTP path
func (tc *TemporalConfig) GetTemporalCertOTPFullPath() string {
	return tc.TemporalCertPath + "/otp"
}

// GetTemporalCACertFilePath - Get Temporal CA Cert File n Path
func (tc *TemporalConfig) GetTemporalCACertFilePath() (string, string) {
	file := "ca.crt"
	return file, tc.TemporalCertPath + "/ca/"
}

// GetTemporalCACertFullPath - Get Temporal CA Cert Full Path
func (tc *TemporalConfig) GetTemporalCACertFullPath() string {
	file := "tls.crt"
	return tc.TemporalCertPath + "/ca/" + file
}

// GetTemporalClientCertFilePath - Get Temporal client Cert File n Path
func (tc *TemporalConfig) GetTemporalClientCertFilePath() ([]string, string) {
	file := []string{"tls.crt", "tls.key"}
	return file, tc.TemporalCertPath + "/client/"
}

// GetTemporalClientCertFullPath - Get Temporal client Cert Full Path
func (tc *TemporalConfig) GetTemporalClientCertFullPath() string {
	file := "tls.crt"
	return tc.TemporalCertPath + "/client/" + file
}

// GetTemporalClientKeyFullPath - Get Temporal client Key Full Path
func (tc *TemporalConfig) GetTemporalClientKeyFullPath() string {
	file := "tls.key"
	return tc.TemporalCertPath + "/client/" + file
}

// CoreGrpcConfig holds configurations for connecting to CoreGrpc server
type CoreGrpcConfig struct {
	Address        string               `json:"coreGrpcAddress"`
	Secure         client.SecureOptions `json:"coreGrpcSecureOptions"`
	SkipServerAuth bool                 `json:"coreGrpcSkipServerAuth"`
	ServerCAPath   string               `json:"coreGrpcCertPath"`
	ClientCertPath string               `json:"coreGrpcClientCertPath"`
	ClientKeyPath  string               `json:"coreGrpcClientKeyPath"`
}

// FlowGrpcConfig holds configurations for connecting to Flow gRPC server
type FlowGrpcConfig struct {
	Enabled        bool                               `json:"flowGrpcEnabled"`
	Address        string                             `json:"flowGrpcAddress"`
	Secure         client.FlowGrpcClientSecureOptions `json:"flowGrpcSecureOptions"`
	SkipServerAuth bool                               `json:"flowGrpcSkipServerAuth"`
	ServerCAPath   string                             `json:"flowGrpcCertPath"`
	ClientCertPath string                             `json:"flowGrpcClientCertPath"`
	ClientKeyPath  string                             `json:"flowGrpcClientKeyPath"`
}

// Config for Site Agent
type Config struct {
	Temporal         TemporalConfig
	CoreGrpc         CoreGrpcConfig
	FlowGrpc         FlowGrpcConfig
	IsMasterPod      bool          `json:"isMasterPod"`
	EnableDebug      bool          `json:"enableDebug"`
	DevMode          bool          `json:"devMode"`
	EnableTLS        bool          `json:"enableTLS"`
	DisableBootstrap bool          `json:"disableBootstrap"`
	BootstrapSecret  string        `json:"bootstrapSecret"` // Path to the bootstrap secret file
	WatcherInterval  time.Duration `json:"watcherInterval"`
	PodNamespace     string        `json:"podNamespace"`
	TemporalSecret   string        `json:"temporalSecret"`
	MetricsPort      string        `json:"metricsPort"`
	MetricsNamespace string        `json:"metricsNamespace"`
	SiteVersion      string        `json:"siteVersion"`
	CloudVersion     string        `json:"cloudVersion"`
	RunningIn        RunInEnvironment
	UtMode           bool
}

// String - json string
func (c *Config) String() string {
	str, err := json.Marshal(c)
	if err != nil {
		return ""
	}
	return string(str)
}

// NewConfType - new config
func NewConfType() *Config {
	// We can set the default config here
	return &Config{
		Temporal: TemporalConfig{},
		CoreGrpc: CoreGrpcConfig{},
		FlowGrpc: FlowGrpcConfig{},
	}
}
