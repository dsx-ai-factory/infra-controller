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

// BuildDSN builds the Data Source Name (DSN) string for connecting to
// the database. User and password are URL-encoded to handle special
// characters such as "@", ":", "/", etc.
// IPv6 hosts may be supplied with or without brackets.
func (c *Config) BuildDSN() string {
	host := c.Host
	if strings.HasPrefix(host, "[") && strings.HasSuffix(host, "]") {
		host = host[1 : len(host)-1]
	}

	u := &url.URL{
		Scheme: "postgres",
		User:   url.UserPassword(c.Credential.User, c.Credential.Password.Value),
		Host:   net.JoinHostPort(host, strconv.Itoa(c.Port)),
		Path:   c.DBName,
	}

	if len(c.CACertificatePath) > 0 {
		// Use sslmode=prefer (like Flow) instead of verify-full to avoid issues with expired server certs
		u.RawQuery = fmt.Sprintf("sslmode=prefer&sslrootcert=%s", c.CACertificatePath)
	} else {
		u.RawQuery = "sslmode=disable"
	}

	return u.String()
}

// BuildDBConfigFromEnv builds a Config from environment variables.
// Port is read from PGPORT, defaulting to 30432 (the CI host-mapped port).
// Optional: DB_HOST, DB_NAME, DB_USER, DB_PASSWORD, DB_CA_CERT_PATH
func BuildDBConfigFromEnv() (Config, error) {
	host := os.Getenv("DB_HOST")
	if host == "" {
		host = "localhost"
	}

	// Default to port 30432 to match the CI PostgreSQL service port mapping
	// (see .github/workflows/lint-and-test.yml ports: 30432:5432).
	// Same convention used in db/pkg/util/testing.go getTestDBParams().
	portStr := os.Getenv("PGPORT")
	if portStr == "" {
		portStr = "30432"
	}
	port, err := strconv.Atoi(portStr)
	if err != nil {
		return Config{}, fmt.Errorf("invalid PGPORT: %v", err)
	}

	dbName := os.Getenv("DB_NAME")
	if dbName == "" {
		dbName = "nvswitch_manager"
	}

	user := os.Getenv("DB_USER")
	if user == "" {
		user = "postgres"
	}

	password := os.Getenv("DB_PASSWORD")
	if password == "" {
		password = "postgres"
	}

	caCertPath := os.Getenv("DB_CA_CERT_PATH")

	config := Config{
		Host:              host,
		Port:              port,
		DBName:            dbName,
		CACertificatePath: caCertPath,
	}
	config.Credential.Update(&user, &password)

	return config, nil
}
