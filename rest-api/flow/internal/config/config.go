// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package config defines a configuration file that allows tweaking values on a per environment basis.
// Default values are used if the file is missing or for individual values that were not specified.

package config

import (
	"os"
	"time"

	"github.com/rs/zerolog/log"
	"gopkg.in/yaml.v3"
)

const defaultLocation = "/etc/flow/flowconfig.yaml"

// Config is a set of configuration operations that will be read from an environment specific config file
type Config struct {
	InventoryRunFrequency time.Duration `yaml:"inventory_run_frequency"`
	GRPCTimeout           time.Duration `yaml:"grpc_timeout"`
	DisableInventory      bool          `yaml:"disable_inventory"`
	LeakDetectionInterval time.Duration `yaml:"leak_detection_interval"`
	DisableLeakDetection  bool          `yaml:"disable_leak_detection"`
	Tracing               TracingConfig `yaml:"tracing"`
}

// TracingConfig controls local span export independently of the OTLP endpoint.
type TracingConfig struct {
	// Enabled defaults to false; trace-context propagation remains active.
	Enabled bool `yaml:"enabled"`
}

// defaultConfig sets up the default values used when something is not specified
func defaultConfig() Config {
	return Config{InventoryRunFrequency: time.Minute,
		GRPCTimeout:           time.Minute,
		LeakDetectionInterval: time.Minute,
		DisableLeakDetection:  false,
	}
}

// ReadConfig reads a configuration file if present and returns a Config with the details.  A config file with
// invalid syntax will result in a fatal error.
func ReadConfig() (config Config) {
	filename := os.Getenv("FLOW_CONFIG_FILE")
	if filename == "" {
		filename = defaultLocation
	}

	file, err := os.Open(filename)
	if err != nil {
		log.Warn().Msgf("Config file %s not found, using defaults.", filename)
		return defaultConfig()
	}
	defer file.Close()

	// Will use default values for anything not specified
	config = defaultConfig()
	if err := yaml.NewDecoder(file).Decode(&config); err != nil {
		log.Fatal().Msgf("Invalid configuration file %s: %v", filename, err)
	}

	return config
}

// UnitTestConfig returns a Config that is suitable for running unit tests.
func UnitTestConfig() Config {
	cfg := defaultConfig()
	cfg.InventoryRunFrequency = time.Millisecond

	return cfg
}
