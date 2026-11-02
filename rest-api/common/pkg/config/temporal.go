// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"crypto/tls"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/endpoint"
	ctls "github.com/NVIDIA/infra-controller/rest-api/common/pkg/tls"
	cwfns "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/namespace"
)

// TemporalConfig holds configuration for Temporal communication
type TemporalConfig struct {
	Host          string
	Port          int
	ServerName    string
	Namespace     string
	Queue         string
	EncryptionKey string
	TLSEnabled    bool
	ClientTLSCfg  *tls.Config
	dynTLS        *ctls.DynTLSCfg
}

// GetHostPort returns the Temporal host:port connection target.
// IPv6 hosts may be supplied with or without brackets.
func (tcfg *TemporalConfig) GetHostPort() string {
	config := endpoint.Config{Host: tcfg.Host, Port: tcfg.Port}
	return config.Target()
}

// Close cleans up TLS resources
func (tcfg *TemporalConfig) Close() {
	if tcfg.dynTLS != nil {
		tcfg.dynTLS.Close()
	}
}

// NewTemporalConfig initializes and returns a configuration object for managing Temporal
func NewTemporalConfig(host string, port int, serverName string, namespace string, queue string, encryptionKey string, tlsEnabled bool, certPath string, keyPath string, caPath string) (*TemporalConfig, error) {
	var dynTLS *ctls.DynTLSCfg
	var clientTLSCfg *tls.Config

	if tlsEnabled {
		var err error

		dynTLS, err = ctls.NewDynTLSCfg(keyPath, certPath, caPath)
		if err != nil {
			return nil, err
		}

		baseTLSCfg := &tls.Config{ //nolint:exhaustruct // only setting relevant TLS fields
			ServerName:         fmt.Sprintf("%s.%s", cwfns.CloudNamespace, serverName),
			MinVersion:         tls.VersionTLS12,
			InsecureSkipVerify: false,
		}

		clientTLSCfg = dynTLS.WithTLSCfg(baseTLSCfg).ClientCfg()
	}

	return &TemporalConfig{
		Host:          host,
		Port:          port,
		Namespace:     namespace,
		Queue:         queue,
		ServerName:    serverName,
		EncryptionKey: encryptionKey,
		TLSEnabled:    tlsEnabled,
		ClientTLSCfg:  clientTLSCfg,
		dynTLS:        dynTLS,
	}, nil
}
