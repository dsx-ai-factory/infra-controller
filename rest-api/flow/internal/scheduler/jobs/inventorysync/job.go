// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"fmt"
	"os"
	"strconv"

	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico" //nolint
)

// envExpectedSyncEnabled gates the expected-inventory mirror that runs at
// the start of each inventory cycle (see expected_mirror*.go). Default is
// "on": an unset or empty value enables the mirror. Accepted values are
// anything strconv.ParseBool accepts (1, t, T, true, True, TRUE, 0, false,
// ...). An unparseable value resolves to disabled with a warning so a
// typo cannot silently leave the gate in an ambiguous state. Set the
// variable to false/0/f to opt out.
const envExpectedSyncEnabled = "FLOW_EXPECTED_INVENTORY_SYNC_ENABLED"

// Job implements scheduler.Job for the inventory sync task.
type Job struct {
	dbConf              *cdb.Config
	nicoClient          nicoapi.Client
	pool                *cdb.Session
	expectedSyncEnabled bool
}

func readExpectedSyncEnabled() bool {
	raw := os.Getenv(envExpectedSyncEnabled)
	if raw == "" {
		return true
	}
	enabled, err := strconv.ParseBool(raw)
	if err != nil {
		log.Warn().
			Str("env", envExpectedSyncEnabled).
			Str("raw", raw).
			Msg("Expected-inventory mirror toggle: env var value is not a boolean (use 1/0/true/false); treating as disabled")
		return false
	}
	return enabled
}

// New constructs an inventory sync Job using clients sourced from the provider
// registry. Returns nil, nil if inventory is disabled or the NICo provider
// is not registered.
func New(
	ctx context.Context,
	dbConf *cdb.Config,
	providers *providerapi.ProviderRegistry,
	cfg config.Config,
) (*Job, error) {
	if cfg.DisableInventory {
		log.Info().Msg("Inventory disabled by configuration")
		return nil, nil
	}

	if dbConf == nil {
		return nil, fmt.Errorf("database configuration is nil")
	}

	nicoProvider, err := providerapi.GetTyped[*nicoprovider.Provider](
		providers, nicoprovider.ProviderName,
	)
	if err != nil {
		log.Warn().
			Err(err).
			Msg("NICo provider not available; inventory sync disabled")
		return nil, nil
	}

	pool, err := cdb.NewSessionFromConfig(ctx, *dbConf)
	if err != nil {
		return nil, fmt.Errorf("failed to create database pool: %w", err)
	}

	// TODO(follow-up PR): several cleanups are deferred to keep this PR focused:
	//  - Resource lifecycle: pool is never explicitly closed. Jobs should
	//    implement a Close() method so the scheduler can release resources on
	//    shutdown. The same applies to the leak-detection job.
	//  - Store abstraction: raw DB access (pool, dbConf) should be hidden behind
	//    a store interface so jobs depend on a domain-level contract rather than
	//    the database session directly.
	//  - Provider encapsulation: the NICo client is wired here by reaching into
	//    the component-manager provider registry. This logic should move into
	//    the component manager so jobs receive ready-to-use domain clients
	//    instead of low-level provider handles.

	expectedSyncEnabled := readExpectedSyncEnabled()
	log.Info().
		Bool("enabled", expectedSyncEnabled).
		Str("env", envExpectedSyncEnabled).
		Msg("Expected-inventory mirror: feature gate resolved at job construction")

	return &Job{
		dbConf:              dbConf,
		nicoClient:          nicoProvider.Client(),
		pool:                pool,
		expectedSyncEnabled: expectedSyncEnabled,
	}, nil
}

// Name returns the job name.
func (j *Job) Name() string { return "inventory-sync" }

// Run executes one iteration of the inventory sync.
// No error is returned because runInventoryOne handles all errors internally:
// each sync step logs failures and continues, and the final drift persistence
// error is also logged rather than propagated. A failed iteration is not
// fatal — the scheduler will simply retry on the next trigger fire.
func (j *Job) Run(ctx context.Context, _ types.Event) error {
	runInventoryOne(ctx, j.pool, j.nicoClient, j.expectedSyncEnabled)
	return nil
}
