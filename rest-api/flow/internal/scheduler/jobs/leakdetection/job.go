// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakdetection

import (
	"context"
	"fmt"

	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/config"
	eventingestion "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/ingestion"
	leakagedetector "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage/detector"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico" //nolint
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type detectorRunner interface {
	Run(context.Context) error
}

type detectorRunnerFunc func(context.Context) error

func (f detectorRunnerFunc) Run(ctx context.Context) error { return f(ctx) }

type reconcileFunc func(context.Context, devicetypes.ComponentType, []string)

type reconcilingReader struct {
	reader    leakagedetector.Reader
	reconcile reconcileFunc
}

func (r reconcilingReader) GetLeakingMachineIds(ctx context.Context) ([]string, error) {
	ids, err := r.reader.GetLeakingMachineIds(ctx)
	if err != nil {
		return nil, err
	}

	r.reconcile(ctx, devicetypes.ComponentTypeCompute, ids)

	return ids, nil
}

func (r reconcilingReader) GetLeakingSwitchIds(ctx context.Context) ([]string, error) {
	ids, err := r.reader.GetLeakingSwitchIds(ctx)
	if err != nil {
		return nil, err
	}

	r.reconcile(ctx, devicetypes.ComponentTypeNVSwitch, ids)

	return ids, nil
}

// Job implements scheduler.Job for the leak detection task.
type Job struct {
	detector detectorRunner
}

// New constructs a leak detection Job using the NICo provider from the
// registry. Returns nil, nil if leak detection is disabled or the NICo
// provider is not registered (e.g. non-production environment).
func New(
	ctx context.Context,
	dbConf *cdb.Config,
	taskMgr taskmanager.Manager,
	providers *providerapi.ProviderRegistry,
	pipeline *eventingestion.Pipeline,
	cfg config.Config,
) (*Job, error) {
	if cfg.DisableLeakDetection {
		log.Info().Msg("Leak detection disabled by configuration")
		return nil, nil
	}

	nicoProvider, err := providerapi.GetTyped[*nicoprovider.Provider](
		providers, nicoprovider.ProviderName,
	)
	if err != nil {
		log.Error().Err(err).
			Msg("NICo provider not available; leak detection disabled")
		return nil, nil
	}

	if dbConf == nil {
		return nil, fmt.Errorf("database configuration is nil")
	}

	pool, err := cdb.NewSessionFromConfig(ctx, *dbConf)
	if err != nil {
		return nil, fmt.Errorf("failed to create database pool: %w", err)
	}

	// A nil pipeline selects the legacy detector during the migration to
	// event-rule ingestion. The selected implementation is fixed for the
	// lifetime of the Job.
	if pipeline == nil {
		return newJob(detectorRunnerFunc(func(ctx context.Context) error {
			runLeakDetectionOne(ctx, nicoProvider.Client(), taskMgr, pool)

			return nil
		}))
	}

	reader := reconcilingReader{
		reader: nicoProvider.Client(),
		reconcile: func(
			ctx context.Context,
			componentType devicetypes.ComponentType,
			ids []string,
		) {
			reconcileLeakStatus(ctx, pool, componentType, ids)
		},
	}

	eventRuleDetector, err := leakagedetector.New(reader)
	if err != nil {
		return nil, fmt.Errorf("create event-rule leakage detector: %w", err)
	}

	eventRuleSource, err := pipeline.RegisterSource(leakagedetector.SourceName)
	if err != nil {
		return nil, fmt.Errorf("register event-rule leakage source: %w", err)
	}

	return newJob(detectorRunnerFunc(func(ctx context.Context) error {
		return eventRuleDetector.Collect(ctx, eventRuleSource.Ingest)
	}))
}

// Name returns the job name.
func (j *Job) Name() string { return "leak-detection" }

// Run executes one iteration of leak detection.
func (j *Job) Run(ctx context.Context, _ types.Event) error {
	return j.detector.Run(ctx)
}

func newJob(detector detectorRunner) (*Job, error) {
	if detector == nil {
		return nil, fmt.Errorf("leakage detector is required")
	}

	return &Job{detector: detector}, nil
}
