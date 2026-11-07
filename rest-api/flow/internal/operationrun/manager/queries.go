// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/google/uuid"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// Get returns one operation run.
func (m *ManagerImpl) Get(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	if err := m.requireDependencies(); err != nil {
		return nil, err
	}
	if id == uuid.Nil {
		return nil, fmt.Errorf("operation run ID is required")
	}

	run, err := m.store.Get(ctx, id)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, fmt.Errorf("%w: %s", ErrOperationRunNotFound, id)
		}
		return nil, err
	}

	return run, nil
}

// List returns operation runs matching opts.
func (m *ManagerImpl) List(
	ctx context.Context,
	opts operationrun.ListOptions,
) ([]*operationrun.OperationRun, int32, error) {
	if err := m.requireDependencies(); err != nil {
		return nil, 0, err
	}

	return m.store.List(ctx, opts)
}

// ListTargets returns materialized targets for one operation run.
func (m *ManagerImpl) ListTargets(
	ctx context.Context,
	id uuid.UUID,
	opts operationrun.TargetListOptions,
) ([]*operationrun.OperationRunTarget, int32, error) {
	if err := m.requireDependencies(); err != nil {
		return nil, 0, err
	}
	if id == uuid.Nil {
		return nil, 0, fmt.Errorf("operation run ID is required")
	}

	targets, total, err := m.store.ListTargets(ctx, id, opts)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, 0, fmt.Errorf("%w: %s", ErrOperationRunNotFound, id)
		}
		return nil, 0, err
	}

	return targets, total, nil
}
