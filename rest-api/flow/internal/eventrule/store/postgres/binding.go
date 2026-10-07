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
