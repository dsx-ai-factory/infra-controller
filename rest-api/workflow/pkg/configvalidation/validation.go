// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package configvalidation validates inline workflow configuration without starting a worker.
package configvalidation

import (
	"errors"
	"fmt"
	"strings"

	"github.com/spf13/viper"
)

const (
	DefaultMaxConcurrentActivityPollers = 10
	MaxConcurrentActivityPollers        = 20
	DefaultMetricsNamespace             = "nico_rest_workflow"
)

// SetDefaults applies the workflow's startup defaults to a request-local Viper instance.
// It does not enable environment overrides or read configuration files.
func SetDefaults(v *viper.Viper) {
	v.SetDefault("log.level", "info")
	v.SetDefault("env.dev", false)
	v.SetDefault("db.host", "localhost")
	v.SetDefault("db.port", 5432)
	v.SetDefault("temporal.host", "localhost")
	v.SetDefault("temporal.port", 7233)
	v.SetDefault("temporal.tls.enabled", true)
	v.SetDefault("metrics.enabled", true)
	v.SetDefault("metrics.port", 9360)
	v.SetDefault("metrics.namespace", DefaultMetricsNamespace)
	v.SetDefault("healthz.enabled", true)
	v.SetDefault("healthz.port", 8899)
	v.SetDefault("tracing.enabled", false)
	v.SetDefault("worker.maxConcurrentActivityPollers", DefaultMaxConcurrentActivityPollers)
}

// Validate checks startup's required fields and poller bounds, without resolving
// secret paths, connecting to services, or modifying global state. Values normally
// injected from environment or secret files must be present in v for this check.
// An encryptionKeyPath is accepted without reading it; startup reads the path
// before validation. Inline validation checks only that a source was supplied.
func Validate(v *viper.Viper) error {
	for _, field := range []struct{ key, message string }{
		{"db.name", "db name config must be specified"},
		{"db.user", "db user config must be specified"},
	} {
		if v.GetString(field.key) == "" {
			return errors.New(field.message)
		}
	}
	if v.GetString("db.password") == "" && v.GetString("db.passwordPath") == "" {
		return errors.New("db password or password path config must be specified")
	}
	if v.GetBool("temporal.tls.enabled") {
		for _, field := range []struct{ key, message string }{
			{"temporal.tls.certPath", "temporal cert path config must be specified"},
			{"temporal.tls.keyPath", "temporal key path config must be specified"},
			{"temporal.tls.caPath", "temporal ca path config must be specified"},
		} {
			if v.GetString(field.key) == "" {
				return errors.New(field.message)
			}
		}
	}
	for _, field := range []struct{ key, message string }{
		{"temporal.serverName", "temporal server name config must be specified"},
		{"temporal.namespace", "temporal namespace config must be specified"},
		{"temporal.queue", "temporal queue config must be specified"},
	} {
		if v.GetString(field.key) == "" {
			return errors.New(field.message)
		}
	}
	if v.GetString("temporal.encryptionKey") == "" && v.GetString("temporal.encryptionKeyPath") == "" {
		return errors.New("temporal encryption key or encryption key path config must be specified")
	}
	if p := v.GetInt("worker.maxConcurrentActivityPollers"); p < 1 || p > MaxConcurrentActivityPollers {
		return fmt.Errorf("worker max concurrent activity pollers %d must be between 1 and %d", p, MaxConcurrentActivityPollers)
	}
	return nil
}

// ValidateYAML validates only the workflow worker's static startup fields in
// inline YAML. It never reads paths (including secrets), applies environment
// overrides, initializes watchers, or starts a service. It does not validate
// credentials, certificate contents, connectivity, or service-specific startup.
func ValidateYAML(input string) error {
	v := viper.New()
	SetDefaults(v)
	v.SetConfigType("yaml")
	if err := v.ReadConfig(strings.NewReader(input)); err != nil {
		return fmt.Errorf("parse workflow YAML: %w", err)
	}
	return Validate(v)
}
