// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	converterdao "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

// Bind stores a rule-to-scope association. Locking the parent rule serializes
// the site-versus-rack check for concurrent bindings of the same rule; partial
// unique indexes serialize conflicts for the same resolved scope.
func (s *Store) Bind(ctx context.Context, binding eventrule.Binding) error {
	if err := binding.Validate(); err != nil {
		return err
	}

	return s.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		_, lockErr := tx.ExecContext(ctx, "SELECT pg_advisory_xact_lock(hashtextextended(?, 0))", scopeLockKey(binding.EventType, binding.Scope))
		if lockErr != nil {
			return lockErr
		}
		var rule dbmodel.EventRule
		err := tx.NewSelect().
			Model(&rule).
			Column("id", "event_type").
			Where("er.id = ?", binding.RuleID).
			For("UPDATE").
			Scan(ctx)
		if errors.Is(err, sql.ErrNoRows) {
			return fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, binding.RuleID)
		}
		if err != nil {
			return err
		}

		if rule.EventType != string(binding.EventType) {
			return fmt.Errorf(
				"event rule binding event type %q does not match rule event type %q",
				binding.EventType,
				rule.EventType,
			)
		}

		conflict, err := bindingForScope(ctx, tx, binding.EventType, binding.Scope)
		if err != nil {
			return err
		}
		if conflict != nil {
			return bindingScopeConflict(binding)
		}

		exists, err := tx.NewSelect().
			Model((*dbmodel.EventRuleBinding)(nil)).
			Where("erb.rule_id = ?", binding.RuleID).
			Where("erb.scope_type <> ?", string(binding.Scope.Type)).
			Exists(ctx)
		if err != nil {
			return err
		}
		if exists {
			return fmt.Errorf(
				"%w: event rule %s cannot mix site and rack bindings",
				eventrule.ErrBindingConflict,
				binding.RuleID,
			)
		}

		now, err := s.timestamp(ctx, tx)
		if err != nil {
			return err
		}

		record, err := converterdao.EventRuleBindingTo(binding, now, now)
		if err != nil {
			return err
		}

		_, err = tx.NewInsert().Model(record).Exec(ctx)
		if err != nil && s.pg.GetErrorChecker().IsUniqueConstraintError(err) {
			return bindingScopeConflict(binding)
		}

		return err
	})
}

// Unbind atomically deletes the binding for an event type and scope.
func (s *Store) Unbind(
	ctx context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) error {
	if err := eventType.Validate(); err != nil {
		return err
	}
	if err := scope.Validate(); err != nil {
		return err
	}

	query := s.pg.DB.NewDelete().
		Model((*dbmodel.EventRuleBinding)(nil)).
		Where("event_type = ?", string(eventType)).
		Where("scope_type = ?", string(scope.Type))
	if scope.Type == eventrule.ScopeTypeSite {
		query = query.Where("scope_id IS NULL")
	} else {
		query = query.Where("scope_id = ?", scope.ID)
	}

	result, err := query.Exec(ctx)
	if err != nil {
		return err
	}

	deleted, err := result.RowsAffected()
	if err != nil {
		return err
	}

	if deleted == 0 {
		return fmt.Errorf(
			"%w: binding for event type %q and scope %q",
			eventrule.ErrBindingNotFound,
			eventType,
			scope.Type,
		)
	}

	return nil
}

// GetForScope returns the binding for an event type and scope.
func (s *Store) GetForScope(
	ctx context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) (*eventrule.Binding, error) {
	if err := eventType.Validate(); err != nil {
		return nil, err
	}
	if err := scope.Validate(); err != nil {
		return nil, err
	}

	return bindingForScope(ctx, s.pg.DB, eventType, scope)
}

func bindingForScope(
	ctx context.Context,
	db bun.IDB,
	eventType eventrule.Type,
	scope eventrule.Scope,
) (*eventrule.Binding, error) {
	var record dbmodel.EventRuleBinding
	query := db.NewSelect().
		Model(&record).
		Where("erb.event_type = ?", string(eventType)).
		Where("erb.scope_type = ?", string(scope.Type))

	if scope.Type == eventrule.ScopeTypeSite {
		query = query.Where("erb.scope_id IS NULL")
	} else {
		query = query.Where("erb.scope_id = ?", scope.ID)
	}

	err := query.Scan(ctx)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}

	return converterdao.EventRuleBindingFrom(&record)
}

func bindingScopeConflict(binding eventrule.Binding) error {
	return fmt.Errorf(
		"%w: event type %q already has a binding for scope %q",
		eventrule.ErrBindingConflict,
		binding.EventType,
		binding.Scope.Type,
	)
}

// PutBinding replaces an explicit assignment, serialized with policy deletion.
func (s *Store) PutBinding(ctx context.Context, binding eventrule.Binding) (*eventrule.BindingPutResult, error) {
	err := binding.Validate()
	if err != nil {
		return nil, err
	}
	var result *eventrule.BindingPutResult
	err = s.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		// Lock the slot before its occupant, matching override writers' lock order.
		_, err := tx.ExecContext(ctx, "SELECT pg_advisory_xact_lock(hashtextextended(?, 0))", scopeLockKey(binding.EventType, binding.Scope))
		if err != nil {
			return err
		}
		var rule dbmodel.EventRule
		err = tx.NewSelect().Model(&rule).Where("er.id = ?", binding.RuleID).For("UPDATE").Scan(ctx)
		if errors.Is(err, sql.ErrNoRows) {
			return eventrule.ErrRuleNotFound
		}
		if err != nil {
			return err
		}
		if rule.EventType != string(binding.EventType) {
			return fmt.Errorf("%w: policy event type does not match binding", eventrule.ErrInvalidRuleInput)
		}
		existing, err := bindingForScope(ctx, tx, binding.EventType, binding.Scope)
		if err != nil {
			return err
		}
		if existing != nil && existing.RuleID == binding.RuleID {
			result = &eventrule.BindingPutResult{Binding: existing}
			return nil
		}
		now, err := s.timestamp(ctx, tx)
		if err != nil {
			return err
		}
		if existing != nil {
			_, err = tx.NewUpdate().Model((*dbmodel.EventRuleBinding)(nil)).Set("rule_id = ?", binding.RuleID).Set("updated_at = ?", now).Where("id = ?", existing.ID).Exec(ctx)
			if err != nil {
				return err
			}
			existing.RuleID = binding.RuleID
			result = &eventrule.BindingPutResult{Binding: existing}
			return nil
		}
		record, err := converterdao.EventRuleBindingTo(binding, now, now)
		if err != nil {
			return err
		}
		_, err = tx.NewInsert().Model(record).Exec(ctx)
		if err != nil {
			return err
		}
		result = &eventrule.BindingPutResult{Binding: &binding, Created: true}
		return nil
	})
	return result, err
}

// ListBindings includes disabled policies' references and orders before slicing.
func (s *Store) ListBindings(ctx context.Context, request eventrule.BindingListRequest) ([]eventrule.Binding, int, error) {
	err := request.Validate()
	if err != nil {
		return nil, 0, err
	}
	var records []dbmodel.EventRuleBinding
	query := s.pg.DB.NewSelect().Model(&records)
	if request.RuleID != uuid.Nil {
		query = query.Where("erb.rule_id = ?", request.RuleID)
	}
	if request.EventType != "" {
		query = query.Where("erb.event_type = ?", string(request.EventType))
	}
	if request.ScopeType != "" {
		query = query.Where("erb.scope_type = ?", string(request.ScopeType))
	}
	if request.RackID != uuid.Nil {
		query = query.Where("erb.scope_type = ? AND erb.scope_id = ?", string(eventrule.ScopeTypeRack), request.RackID)
	}
	total, err := query.OrderExpr("CASE WHEN erb.scope_type = 'site' THEN 0 ELSE 1 END, erb.scope_id ASC, erb.event_type ASC, erb.id ASC").Offset(request.Offset).Limit(request.Limit).ScanAndCount(ctx)
	if err != nil {
		return nil, 0, err
	}
	result := make([]eventrule.Binding, 0, len(records))
	for i := range records {
		value, err := converterdao.EventRuleBindingFrom(&records[i])
		if err != nil {
			return nil, 0, err
		}
		result = append(result, *value)
	}
	return result, total, nil
}

func scopeLockKey(eventType eventrule.Type, scope eventrule.Scope) string {
	return fmt.Sprintf("%s/%s/%s", eventType, scope.Type, scope.ID)
}
