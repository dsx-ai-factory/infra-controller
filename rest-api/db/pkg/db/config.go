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
	// SSLMode is the libpq connection TLS mode. Empty means "prefer".
	SSLMode string
}

// validSSLModes are the libpq sslmode values pgx accepts in a DSN.
var validSSLModes = map[string]struct{}{
	"disable":     {},
	"allow":       {},
	"prefer":      {},
	"require":     {},
	"verify-ca":   {},
	"verify-full": {},
}

// ValidateSSLMode returns an error unless mode is a libpq sslmode value pgx
// accepts. The empty string is left to the caller's default.
func ValidateSSLMode(mode string) error {
	_, ok := validSSLModes[mode]
	if !ok {
		return fmt.Errorf("invalid SSL mode %q: must be one of disable, allow, prefer, require, verify-ca, verify-full", mode)
	}

	return nil
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

	if c.SSLMode != "" {
		if err := ValidateSSLMode(c.SSLMode); err != nil {
			return err
		}
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

	cfg := Config{
		Host:              os.Getenv("DB_HOST"),
		Port:              port,
		Credential:        cred,
		DBName:            os.Getenv("DB_NAME"),
		CACertificatePath: os.Getenv("DB_CERT_PATH"),
	}
	if err := cfg.Validate(); err != nil {
		return Config{}, err
	}

	return cfg, nil
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

	// An unset SSLMode keeps the historical default: `sslmode=disable` broke
	// hostssl-only servers in v1.3.1, so `prefer` stays explicit to let a
	// missing CA path still allow TLS negotiation.
	sslMode := c.SSLMode
	if sslMode == "" {
		sslMode = "prefer"
	}
	if len(c.CACertificatePath) > 0 {
		dsn += fmt.Sprintf("%v&sslrootcert=%v", sslMode, c.CACertificatePath)
	} else {
		dsn += sslMode
	}

	return dsn
}
