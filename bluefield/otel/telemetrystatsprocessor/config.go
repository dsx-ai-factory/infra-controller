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

package telemetrystatsprocessor

import (
	"errors"
	"fmt"
	"time"

	"go.opentelemetry.io/collector/component"
)

// Config defines the configuration of the telemetry_stats processor.
type Config struct {
	// MetricGroupings configure which grouping or groupings of metrics
	// are counted, if any.
	MetricGroupings []MetricGrouping `mapstructure:"metric_groupings"`

	// ScrapeInterval configures how often accumulated counts are "scraped"
	// to generate telemetry_stats datapoints for insertion into the
	// current metrics pipeline, and is only needed if `metric_groupings`
	// is configured. Defaults to "1m".
	MetricScrapeInterval time.Duration `mapstructure:"metric_scrape_interval"`

	// LogGroupings configure which grouping or groupings of logs are
	// counted, if any.
	LogGroupings []LogGrouping `mapstructure:"log_groupings"`

	// LogStatsPort configures the local port of the prometheus endpoint
	// http://localhost:<port>/metrics where log stats can be scraped by a
	// prometheus receiver (log stats cannot be inserted into the current
	// logs pipeline), and is only needed if `log_groupings` is configured.
	LogStatsPort int `mapstructure:"log_stats_port"`

	// LogStatsEndpoint provides a way to configure a prometheus endpoint
	// such as "localhost:<port>" for log stats instead of the one
	// resulting from `log_stats_port`.
	LogStatsEndpoint string `mapstructure:"log_stats_endpoint"`

	// Labels is an optional list of labels to add to all telemetry stats
	// as resource attributes.
	Labels []Label `mapstructure:"labels"`

	// IncludeTelemetryStats configures whether reported stats should
	// include self reporting about telemetry_stats exactly like reporting
	// about processed metric datapoints.
	IncludeTelemetryStats bool `mapstructure:"include_telemetry_stats"`
}

// ensure that Config implements the component.Config interface
var _ component.Config = (*Config)(nil)

// MetricGrouping defines a single grouping of metrics about metrics.
type MetricGrouping struct {
	// Name is the grouping name that appears as a datapoint attribute
	// `grouping="<name>"` on generated metric stats.
	Name string `mapstructure:"name"`

	// ByMetricName configures whether metrics are counted by name, and it
	// appears as a datapoint attribute `metric_name="<name>"` on generated
	// stats.
	ByMetricName bool `mapstructure:"by_metric_name"`

	// ByMetricName configures whether metrics are counted by type
	// (Counter, Gauge, Histogram, or Summary), and it appears as a
	// datapoint attribute `metric_type="<type>"` on generated stats.
	ByMetricType bool `mapstructure:"by_metric_type"`

	// ByLabel configures whether metrics are counted by distinct values of
	// labels applied earlier in the pipeline, and they appear as datapoint
	// attributes `<label-name>="<label-value>"` on generated stats.
	ByLabel *ByLabel `mapstructure:"by_label"`

	// Include configures a filter that limits which metrics are included
	// in the grouping. If unspecified, all metrics are included.
	Include *MetricFilter `mapstructure:"include"`

	// Exclude configures a filter that specifies metrics to exclude from
	// the grouping. If unspecified, no metrics are excluded.
	Exclude *MetricFilter `mapstructure:"exclude"`
}

// LogGrouping defines a single grouping of metrics about logs.
type LogGrouping struct {
	// Name is the grouping name that appears as a log record attribute
	// `grouping="<name>"` on generated log stats.
	Name string `mapstructure:"name"`

	// ByLabel configures whether logs are counted by distinct values of
	// labels applied earlier in the pipeline, and they appear as log
	// record attributes `<label-name>="<label-value>"` on generated
	// stats.
	ByLabel *ByLabel `mapstructure:"by_label"`
}

// ByLabel defines which labels to group by.
type ByLabel struct {
	// Names are the label names specified by `metric_groupings.by_label`
	// and `log_groupings.by_label`.
	Names []string `mapstructure:"names"`
}

// MetricFilter defines criteria to limit which metrics are included in the grouping.
type MetricFilter struct {
	// MetricNames is a list of metric names to filter by.
	MetricNames []string `mapstructure:"metric_names"`
	// MetricRegex is a regular expression that matches metric names to filter by.
	MetricRegex string `mapstructure:"metric_regex"`
	// MetricTypes is a list of metric types (Counter, Gauge, Histogram, or
	// Summary) to filter by.
	MetricTypes []string `mapstructure:"metric_types"`
	// Labels is a list of label name and values to filter by.
	Labels []LabelFilter `mapstructure:"labels"`
}

// Label defines a label as a key-value pair.
type Label struct {
	// Name is the label name
	Name string `mapstructure:"name"`
	// Value is the label value
	Value string `mapstructure:"value"`
}

// LabelFilter defines label criteria to limit which metrics are included in
// the grouping.
type LabelFilter struct {
	// Name is the label name
	Name string `mapstructure:"name"`
	// Values is a list of label values to filter by.
	Values []string `mapstructure:"values"`
	// ValueRegex is a regular expression that matches label values to filter by.
	ValueRegex string `mapstructure:"value_regex"`
}

// Validate implements the component.Config interface by checking whether the
// configuration is valid.
func (cfg *Config) Validate() error {
	if len(cfg.MetricGroupings) == 0 && len(cfg.LogGroupings) == 0 {
		return errors.New("at least one metric or log grouping must be configured")
	}
	if len(cfg.MetricGroupings) > 0 {
		if cfg.MetricScrapeInterval <= 0 {
			return errors.New("metric_scrape_interval must be positive when metric " +
				"groupings are configured")
		}
	}
	if len(cfg.LogGroupings) > 0 {
		if cfg.LogStatsEndpoint == "" && cfg.LogStatsPort == 0 {
			return errors.New("either log_stats_endpoint or log_stats_port " +
				"must be specified when log groupings are configured")
		}
		if cfg.LogStatsEndpoint != "" && cfg.LogStatsPort != 0 {
			return errors.New("only one of log_stats_endpoint or " +
				"log_stats_port should be specified")
		}
	}
	for _, g := range cfg.MetricGroupings {
		if g.Name == "" {
			return errors.New("grouping name cannot be empty")
		}
	}
	for _, g := range cfg.LogGroupings {
		if g.Name == "" {
			return errors.New("grouping name cannot be empty")
		}
	}
	return nil
}

// GetLogStatsEndpoint gets the prometheus endpoint resulting from
// `log_stats_port` or set by `log_stats_endpoint`.
func (cfg *Config) GetLogStatsEndpoint() string {
	if cfg.LogStatsEndpoint != "" {
		return cfg.LogStatsEndpoint
	}
	if cfg.LogStatsPort != 0 {
		return fmt.Sprintf("localhost:%d", cfg.LogStatsPort)
	}
	return ""
}

func createDefaultConfig() component.Config {
	return &Config{
		MetricGroupings:      []MetricGrouping{},
		MetricScrapeInterval: 1 * time.Minute,
		LogGroupings:         []LogGrouping{},
		Labels:               []Label{},
	}
}
