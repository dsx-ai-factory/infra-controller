/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package puntstatsreceiver

import (
	"errors"
	"time"

	"go.opentelemetry.io/collector/component"
)

// Config defines the configuration of the punt_stats receiver.
type Config struct {
	// Path to the file containing the punt stats to read.
	FilePath string `mapstructure:"file_path"`

	// Optional name of a container with the punt stats file.
	// An empty string means read from the DPU filesystem directly.
	ContainerName string `mapstructure:"container_name"`

	// Scrape interval, e.g. "2s".
	ScrapeInterval time.Duration `mapstructure:"scrape_interval"`
}

// ensure that Config implements the `component.Config` interface
var _ component.Config = (*Config)(nil)

// Validate implements the `component.Config` interface by checking whether the
// configuration is valid.
func (cfg *Config) Validate() error {
	if cfg.FilePath == "" {
		return errors.New("file_path must be set")
	}
	if cfg.ScrapeInterval <= 0 {
		return errors.New("scrape_interval must be positive")
	}
	return nil
}

func createDefaultConfig() component.Config {
	return &Config {
		FilePath:         "/cumulus/nl2docad/run/stats/punt",
		ContainerName:    "",
		ScrapeInterval:   5 * time.Second,
	}
}
