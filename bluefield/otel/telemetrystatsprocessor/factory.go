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
	"context"
	"fmt"

	"go.opentelemetry.io/collector/component"
	"go.opentelemetry.io/collector/consumer"
	"go.opentelemetry.io/collector/processor"
	"go.opentelemetry.io/collector/processor/processorhelper"
)

const (
	typeStr       = "telemetry_stats"
	prefixStr     = typeStr + "_"
	ProcessorName = "telemetrystatsprocessor"
	stability     = component.StabilityLevelAlpha
)

var sourceStr string = fmt.Sprintf("%s:%s", ProcessorName, Version)
var processorCapabilities = consumer.Capabilities{MutatesData: true}

// prefixes telemetry_stats metric names with the processor
func telemetryStatName(name string) string {
	return prefixStr + name
}

func NewFactory() processor.Factory {
	return processor.NewFactory(
		component.MustNewType(typeStr),
		createDefaultConfig,
		processor.WithMetrics(createMetricsProcessor, stability),
		processor.WithLogs(createLogsProcessor, stability),
	)
}

func createMetricsProcessor(
	ctx context.Context,
	set processor.Settings,
	cfg component.Config,
	nextConsumer consumer.Metrics,
) (processor.Metrics, error) {
	p, err := newTelemetryStatsProcessor(cfg.(*Config), set.Logger)
	if err != nil {
		return nil, err
	}

	return processorhelper.NewMetrics(
		ctx,
		set,
		cfg,
		nextConsumer,
		p.processMetrics,
		processorhelper.WithCapabilities(processorCapabilities),
		processorhelper.WithShutdown(func(context.Context) error {
			p.cleanup()
			return nil
		}))
}

func createLogsProcessor(
	ctx context.Context,
	set processor.Settings,
	cfg component.Config,
	nextConsumer consumer.Logs,
) (processor.Logs, error) {
	p, err := newTelemetryStatsProcessor(cfg.(*Config), set.Logger)
	if err != nil {
		return nil, err
	}

	return processorhelper.NewLogs(
		ctx,
		set,
		cfg,
		nextConsumer,
		p.processLogs,
		processorhelper.WithCapabilities(processorCapabilities),
		processorhelper.WithShutdown(func(context.Context) error {
			p.cleanup()
			return nil
		}))
}
