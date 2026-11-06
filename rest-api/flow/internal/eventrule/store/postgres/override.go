// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"fmt"

	converterdao "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

type rackPolicyRow struct {
	dbmodel.EventRule `bun:"embed"`
	RackUUID          uuid.UUID `bun:"rack_uuid"`
	RackID            *string   `bun:"rack_external_id"`
}

// ListRackOverrides reads policy bindings rather than rule definitions: a rule
// shared by several Racks contributes one entry for each configured Rack.
func (s *Store) ListRackOverrides(ctx context.Context, eventType eventrule.Type, offset, limit int) ([]eventrule.ScopedPolicy, int, error) {
	query := s.pg.DB.NewSelect().TableExpr("event_rule_bindings AS erb").
		Join("JOIN event_rules AS er ON er.id = erb.rule_id").
		Join("LEFT JOIN rack AS r ON r.id = erb.scope_id").
		Where("erb.event_type = ?", eventType).
		Where("erb.scope_type = ?", eventrule.ScopeTypeRack).
		Where("er.enabled = TRUE")
	total, err := query.Count(ctx)
	if err != nil {
		return nil, 0, err
	}
	policies := make([]eventrule.ScopedPolicy, 0)
	if limit == 0 || offset >= total {
		return policies, total, nil
	}
	var rows []rackPolicyRow
	err = query.Column("er.id", "er.name", "er.description", "er.enabled", "er.event_type", "er.policy", "er.created_at", "er.updated_at").ColumnExpr("erb.scope_id AS rack_uuid").
		ColumnExpr("NULLIF(r.external_id, '') AS rack_external_id").
		OrderExpr("NULLIF(r.external_id, '') ASC NULLS LAST, erb.scope_id ASC").
		Offset(offset).Limit(limit).Scan(ctx, &rows)
	if err != nil {
		return nil, 0, err
	}
	for _, row := range rows {
		rule, err := converterdao.EventRuleFrom(&row.EventRule)
		if err != nil {
			return nil, 0, err
		}
		policies = append(policies, eventrule.ScopedPolicy{
			Rule: rule, Scope: eventrule.Scope{Type: eventrule.ScopeTypeRack, ID: row.RackUUID}, RackID: row.RackID,
		})
	}
	return policies, total, nil
}

func (s *Store) SetRuleOverride(
	ctx context.Context,
	rule *eventrule.Rule,
	binding eventrule.Binding,
) (*eventrule.Rule, error) {
	if rule == nil {
		return nil, fmt.Errorf("scope rule is required")
	}

	if err := rule.Validate(); err != nil {
		return nil, err
	}

	if err := binding.Validate(); err != nil {
		return nil, err
	}

	// Prepare the persisted rule before opening the transaction so conversion
	// failures do not acquire the override-slot lock.
	candidate, err := converterdao.EventRuleTo(rule)
	if err != nil {
		return nil, err
	}
	lockKey := scopeLockKey(binding.EventType, binding.Scope)

	var result *eventrule.Rule
	err = s.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		// The transaction-scoped lock serializes an otherwise empty policy slot.
		_, err := tx.ExecContext(
			ctx,
			"SELECT pg_advisory_xact_lock(hashtextextended(?, 0))",
			lockKey,
		)
		if err != nil {
			return err
		}

		// Resolve the current occupant only after locking the slot so concurrent
		// writers cannot both observe it as empty.
		existing, err := bindingForScope(ctx, tx, binding.EventType, binding.Scope)
		if err != nil {
			return err
		}

		now, err := s.timestamp(ctx, tx)
		if err != nil {
			return err
		}

		// Replace the rule currently occupying this override slot. Lock it while
		// deciding whether it can be updated in place or requires copy-on-write.
		if existing != nil {
			var record dbmodel.EventRule
			err = tx.NewSelect().Model(&record).Where("er.id = ?", existing.RuleID).For("UPDATE").Scan(ctx)
			if err != nil {
				return err
			}

			shared, err := tx.NewSelect().
				Model((*dbmodel.EventRuleBinding)(nil)).
				Where("erb.rule_id = ?", existing.RuleID).
				Where("erb.id <> ?", existing.ID).
				Exists(ctx)
			if err != nil {
				return err
			}

			if shared {
				// Case 1: Multiple bindings own the existing rule. Use copy-on-write
				// so replacing this override does not alter another scope.
				err = tx.NewInsert().Model(candidate).
					Column("id", "name", "description", "enabled", "event_type", "policy").
					Returning("?Columns").Scan(ctx)
				if err != nil {
					return err
				}
				_, err = tx.NewUpdate().
					Model((*dbmodel.EventRuleBinding)(nil)).
					Set("rule_id = ?", candidate.ID).
					Set("updated_at = ?", now).
					Where("id = ?", existing.ID).
					Exec(ctx)
				if err != nil {
					return err
				}

				result, err = converterdao.EventRuleFrom(candidate)
				return err
			}

			// Case 2: This binding is the existing rule's only owner. Preserve the
			// rule identity and update it in place, keeping updated_at unchanged for
			// an identical write.
			err = tx.NewUpdate().Model(&record).
				Set("name = ?", rule.Name).
				Set("description = ?", rule.Description).
				Set("policy = ?", candidate.Policy).
				Set("enabled = TRUE").
				Set("updated_at = CASE WHEN name = ? AND description = ? AND policy = ? AND enabled THEN updated_at ELSE ? END",
					rule.Name, rule.Description, candidate.Policy, now).
				WherePK().Returning("?TableColumns").Scan(ctx)
			if err != nil {
				return err
			}

			result, err = converterdao.EventRuleFrom(&record)
			return err
		}

		// Case 3: The override slot is empty. Create its rule and binding in the
		// same transaction.
		err = tx.NewInsert().Model(candidate).
			Column("id", "name", "description", "enabled", "event_type", "policy").
			Returning("?Columns").Scan(ctx)
		if err != nil {
			return err
		}

		bindingRecord, err := converterdao.EventRuleBindingTo(binding, now, now)
		if err != nil {
			return err
		}

		if _, err = tx.NewInsert().Model(bindingRecord).Exec(ctx); err != nil {
			return err
		}

		result, err = converterdao.EventRuleFrom(candidate)
		return err
	})

	return result, err
}

func (s *Store) DeleteRuleOverride(
	ctx context.Context,
	eventType eventrule.Type,
	scope eventrule.Scope,
) error {
	lockKey := scopeLockKey(eventType, scope)

	return s.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		// Serialize deletion with writers targeting the same override slot.
		_, err := tx.ExecContext(
			ctx,
			"SELECT pg_advisory_xact_lock(hashtextextended(?, 0))",
			lockKey,
		)
		if err != nil {
			return err
		}

		// Resolve the current occupant only after locking the slot so its binding
		// cannot change while the deletion path is selected.
		binding, err := bindingForScope(ctx, tx, eventType, scope)
		if err != nil {
			return err
		}

		// Case 3: The override slot is empty. Deletion is idempotent.
		if binding == nil {
			return nil
		}

		// Lock the parent rule before checking its bindings. Generic Bind calls
		// use the same lock, so no new owner can be added after this transaction
		// decides whether to delete only the binding or the rule itself.
		var record dbmodel.EventRule
		err = tx.NewSelect().
			Model(&record).
			Column("id").
			Where("er.id = ?", binding.RuleID).
			For("UPDATE").
			Scan(ctx)
		if err != nil {
			return err
		}

		shared, err := tx.NewSelect().
			Model((*dbmodel.EventRuleBinding)(nil)).
			Where("erb.rule_id = ?", binding.RuleID).
			Where("erb.id <> ?", binding.ID).
			Exists(ctx)
		if err != nil {
			return err
		}

		if shared {
			// Case 1: Multiple bindings own the existing rule. Delete only this
			// slot's binding so the rule remains available to the other scopes.
			_, err = tx.NewDelete().
				Model((*dbmodel.EventRuleBinding)(nil)).
				Where("id = ?", binding.ID).
				Exec(ctx)
			return err
		}

		// Case 2: This binding is the existing rule's only owner. Delete the rule;
		// its foreign-key cascade also removes the binding.
		_, err = tx.NewDelete().Model(&record).WherePK().Exec(ctx)
		return err
	})
}

func scopeLockKey(eventType eventrule.Type, scope eventrule.Scope) string {
	return fmt.Sprintf("%s/%s/%s", eventType, scope.Type, scope.ID)
}
