// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"errors"
	"net"
	"strconv"
	"strings"

	pkgcerts "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/certs"
)

// Config represents the configuration needed to create a new Flow service
// gRPC client.
type Config struct {
	Host       string
	Port       int
	ServerName string // overrides the server name used for TLS SNI and certificate verification

	// CertConfig holds certificate file paths for mTLS. Either all three
	// fields must be set (mTLS enabled) or all must be empty (insecure).
	// Providing only some is a validation error.
	CertConfig pkgcerts.Config
}

// Validate checks if the config fields are set correctly.
func (c *Config) Validate() error {
	if c.Host == "" {
		return errors.New("host is required")
	}

	if c.Port <= 0 || c.Port > 65535 {
		return errors.New("port must be within (0, 65535]")
	}

	return c.CertConfig.Validate()
}

// Target builds the target string for connecting to Flow gRPC server.
// IPv6 hosts may be supplied with or without brackets.
func (c *Config) Target() string {
	host := c.Host
	if strings.HasPrefix(host, "[") && strings.HasSuffix(host, "]") {
		host = host[1 : len(host)-1]
	}
	return net.JoinHostPort(host, strconv.Itoa(c.Port))
}
