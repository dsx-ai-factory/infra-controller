// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"errors"
	"fmt"
	"net"
	"net/url"
	"os"
	"strconv"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

// Config represents the configuration needed to connect to a database.
type Config struct {
	Host              string
	Port              int
	DBName            string
	Credential        credential.Credential
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

	if c.DBName == "" {
		return errors.New("database name is required")
	}

	if !c.Credential.IsValid() {
		return errors.New("valid credential is required")
	}

	return nil
}

// ConfigFromEnv builds a Config from environment variables.
// Reads: DB_HOST, DB_PORT, DB_USER, DB_PASSWORD, DB_NAME,
// DB_CERT_PATH (optional CA certificate).
func ConfigFromEnv() (Config, error) {
	port, err := strconv.Atoi(os.Getenv("DB_PORT"))
	if err != nil {
		return Config{}, ErrInvalidPort
	}

	cred := credential.NewFromEnv("DB_USER", "DB_PASSWORD")
	if !cred.IsValid() {
		return Config{}, ErrInvalidCredential
	}

	return Config{
		Host:              os.Getenv("DB_HOST"),
		Port:              port,
		Credential:        cred,
		DBName:            os.Getenv("DB_NAME"),
		CACertificatePath: os.Getenv("DB_CERT_PATH"),
	}, nil
}

// BuildDSN builds the Data Source Name (DSN) string for connecting to
// the database. IPv6 hosts may be supplied with or without brackets.
func (c *Config) BuildDSN() string {
	host := c.Host
	if strings.HasPrefix(host, "[") && strings.HasSuffix(host, "]") {
		host = host[1 : len(host)-1]
	}

	dsn := fmt.Sprintf(
		"postgres://%v:%v@%v/%v?sslmode=",
		url.PathEscape(c.Credential.User),
		url.PathEscape(c.Credential.Password.Value),
		net.JoinHostPort(host, strconv.Itoa(c.Port)),
		c.DBName,
	)

	// `sslmode=disable` broke hostssl-only servers in v1.3.1. Keep `prefer`
	// explicit so a missing CA path still allows TLS negotiation.
	if len(c.CACertificatePath) > 0 {
		dsn += fmt.Sprintf("prefer&sslrootcert=%v", c.CACertificatePath)
	} else {
		dsn += "prefer"
	}

	return dsn
}
