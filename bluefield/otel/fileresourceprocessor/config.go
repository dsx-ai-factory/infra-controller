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

package fileresourceprocessor

import (
	"errors"
	"time"

	"go.opentelemetry.io/collector/component"
)

type Config struct {
	// FilePaths configured files from which to read resource attributes
	FilePaths []string `mapstructure:"file_paths"`

	// PollInterval how often to try reading the configured file until successful
	PollInterval time.Duration `mapstructure:"poll_interval"`
}

var _ component.Config = (*Config)(nil)

func (c *Config) Validate() error {
	if len(c.FilePaths) == 0 {
		return errors.New("at least one file must be configured")
	}
	for _, path := range c.FilePaths {
		if path == "" {
			return errors.New("file path cannot be empty")
		}
	}
	if c.PollInterval <= 0 {
		return errors.New("poll_interval must be positive")
	}
	return nil
}

func createDefaultConfig() component.Config {
	return &Config{
		FilePaths:    []string{},
		PollInterval: 1 * time.Minute,
	}
}
