// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"

	converterdao "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/uptrace/bun"
)

// ObserveEvent records and returns an existing source event. Missing events
// are represented by (nil, nil).
func (s *Store) ObserveEvent(
	ctx context.Context,
	key eventrule.EventKey,
) (*eventrule.Event, error) {
	if err := key.Validate(); err != nil {
		return nil, err
	}

	now, err := s.timestamp(ctx, s.pg.DB)
	if err != nil {
		return nil, err
	}

	return observeEvent(ctx, s.pg.DB, key, now)
}

// CommitEventPlan atomically inserts an event and all immutable action plans.
// A duplicate source event records another observation without changing or
// resuming its existing executions.
func (s *Store) CommitEventPlan(
	ctx context.Context,
	definition eventrule.Event,
	planned []eventrule.PlannedExecution,
) (*eventrule.Event, error) {
	if err := validateEventPlan(definition, planned); err != nil {
		return nil, err
	}

	var created *eventrule.Event
	err := s.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		now, err := s.timestamp(ctx, tx)
		if err != nil {
			return err
		}

		event, err := eventrule.NewEvent(definition, now)
		if err != nil {
			return err
		}

		record, err := converterdao.EventTo(event)
		if err != nil {
			return err
		}

		result, err := tx.NewInsert().
			Model(record).
			On("CONFLICT (source_name, source_key) DO NOTHING").
			Exec(ctx)
		if err != nil {
			return err
		}

		inserted, err := result.RowsAffected()
		if err != nil {
			return err
		}
		if inserted == 0 {
			_, err = observeEvent(ctx, tx, definition.Key, now)

			return err
		}

		executions := make([]dbmodel.EventActionExecution, len(planned))
		for i, item := range planned {
			execution, err := eventrule.NewExecution(
				event.ID,
				item.ActionName,
				item.ExecutionPlan,
				now,
			)
			if err != nil {
				return fmt.Errorf("create action %q execution: %w", item.ActionName, err)
			}

			executionRecord, err := converterdao.EventActionExecutionTo(execution)
			if err != nil {
				return fmt.Errorf("convert action %q execution: %w", item.ActionName, err)
			}

			executions[i] = *executionRecord
		}

		if len(executions) > 0 {
			_, err = tx.NewInsert().Model(&executions).Exec(ctx)
			if err != nil {
				if s.pg.GetErrorChecker().IsUniqueConstraintError(err) {
					return fmt.Errorf(
						"%w: event %s",
						eventrule.ErrExecutionAlreadyExists,
						event.ID,
					)
				}

				return err
			}
		}

		created = event

		return nil
	})
	if err != nil {
		return nil, err
	}

	return created, nil
}

func observeEvent(
	ctx context.Context,
	db bun.IDB,
	key eventrule.EventKey,
	now time.Time,
) (*eventrule.Event, error) {
	var record dbmodel.Event
	err := db.NewUpdate().
		Model(&record).
		Set("observations = e.observations + 1").
		Set("last_observed_at = GREATEST(e.last_observed_at, ?)", now).
		Where("e.source_name = ?", key.SourceName).
		Where("e.source_key = ?", key.SourceKey).
		Returning("e.*").
		Scan(ctx)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}

	return converterdao.EventFrom(&record)
}

func validateEventPlan(
	definition eventrule.Event,
	planned []eventrule.PlannedExecution,
) error {
	if err := definition.ValidateDefinition(); err != nil {
		return err
	}

	if len(planned) != len(definition.EffectivePolicy.Actions) {
		return fmt.Errorf(
			"event plan has %d executions for %d applicable actions",
			len(planned),
			len(definition.EffectivePolicy.Actions),
		)
	}

	seen := make(map[string]struct{}, len(planned))
	for i, item := range planned {
		if err := item.Validate(); err != nil {
			return fmt.Errorf("planned executions[%d]: %w", i, err)
		}

		if _, exists := seen[item.ActionName]; exists {
			return fmt.Errorf(
				"planned executions[%d]: duplicate action name %q",
				i,
				item.ActionName,
			)
		}
		seen[item.ActionName] = struct{}{}

		action := definition.EffectivePolicy.Actions[i]
		if item.ActionName != action.Name {
			return fmt.Errorf(
				"planned executions[%d] has action name %q, want %q",
				i,
				item.ActionName,
				action.Name,
			)
		}

		if item.ExecutionPlan.Type() != action.Spec.Type() {
			return fmt.Errorf(
				"planned executions[%d] has type %q, want %q",
				i,
				item.ExecutionPlan.Type(),
				action.Spec.Type(),
			)
		}
	}

	return nil
}
