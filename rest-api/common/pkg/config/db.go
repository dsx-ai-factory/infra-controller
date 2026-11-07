// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import "fmt"

// DBConfig holds configuration for database access
type DBConfig struct {
	Host     string
	Port     int
	Name     string
	User     string
	Password string
}

// NewDBConfig initializes and returns a configuration object for managing database access
func NewDBConfig(host string, port int, name string, user string, password string) *DBConfig {
	return &DBConfig{
		Host:     host,
		Port:     port,
		Name:     name,
		User:     user,
		Password: password,
	}
}

// GetHostPort returns the concatenated host & port.
func (dbcfg *DBConfig) GetHostPort() string {
	return fmt.Sprintf("%v:%v", dbcfg.Host, dbcfg.Port)
}
