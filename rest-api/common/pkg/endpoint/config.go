// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package endpoint

import (
	"errors"
	"net"
	"strconv"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

// Config represents a network endpoint with optional authentication and TLS.
type Config struct {
	Host              string
	Port              int
	Credential        *credential.Credential
	CACertificatePath string
}

// Validate checks if the Config fields are set correctly.
func (c *Config) Validate() error {
	if c.Host == "" {
		return errors.New("host is required")
	}

	if c.Port <= 0 || c.Port > 65535 {
		return errors.New("port must be between (0, 65535]")
	}

	if c.Credential != nil && !c.Credential.IsValid() {
		return errors.New("valid credential is required")
	}

	return nil
}

// Target returns the host:port connection string.
// IPv6 hosts may be supplied with or without brackets.
func (c *Config) Target() string {
	host := c.Host
	if strings.HasPrefix(host, "[") && strings.HasSuffix(host, "]") {
		host = host[1 : len(host)-1]
	}
	return net.JoinHostPort(host, strconv.Itoa(c.Port))
}
