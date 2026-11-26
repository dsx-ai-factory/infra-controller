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

// GetByID returns one persisted rule.
func (s *Store) GetByID(
	ctx context.Context,
	id uuid.UUID,
) (*eventrule.Rule, error) {
	var record dbmodel.EventRule
	err := s.pg.DB.NewSelect().
		Model(&record).
		Where("er.id = ?", id).
		Scan(ctx)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, id)
	}
	if err != nil {
		return nil, err
	}

	return converterdao.EventRuleFrom(&record)
}

// List returns persisted rules matching the filter in stable ID order.
func (s *Store) List(
	ctx context.Context,
	request eventrule.RuleListRequest,
) (eventrule.RuleListPage, error) {
	if err := request.Validate(); err != nil {
		return eventrule.RuleListPage{}, err
	}

	var total int
	var records []dbmodel.EventRule
	// Count and page must share a snapshot, including binding-based eligibility.
	// READ COMMITTED would take a new snapshot for each statement.
	err := s.pg.DB.RunInTx(ctx, &sql.TxOptions{
		Isolation: sql.LevelRepeatableRead,
		ReadOnly:  true,
	}, func(ctx context.Context, tx bun.Tx) error {
		countQuery := eventRuleListQuery(
			tx.NewSelect().Model((*dbmodel.EventRule)(nil)),
			request.Filter,
		)
		count, err := countQuery.Count(ctx)
		if err != nil {
			return err
		}
		total = count

		query := eventRuleListQuery(tx.NewSelect().Model(&records), request.Filter)
		return query.
			OrderExpr("er.id ASC").
			Offset(request.Offset).
			Limit(request.Limit).
			Scan(ctx)
	})
	if err != nil {
		return eventrule.RuleListPage{}, err
	}

	rules := make([]*eventrule.Rule, len(records))
	for i := range records {
		rule, err := converterdao.EventRuleFrom(&records[i])
		if err != nil {
			return eventrule.RuleListPage{}, err
		}

		rules[i] = rule
	}

	return eventrule.RuleListPage{Rules: rules, Total: total}, nil
}

func eventRuleListQuery(
	query *bun.SelectQuery,
	filter eventrule.RuleFilter,
) *bun.SelectQuery {
	if filter.EventType != nil {
		query = query.Where("er.event_type = ?", string(*filter.EventType))
	}
	if filter.Bound != nil {
		predicate := "EXISTS (SELECT 1 FROM event_rule_bindings AS binding WHERE binding.rule_id = er.id)"
		if !*filter.Bound {
			predicate = "NOT " + predicate
		}
		query = query.Where(predicate)
	}
	if filter.Enabled != nil {
		query = query.Where("er.enabled = ?", *filter.Enabled)
	}

	return query
}

// Create stores a new persisted rule using database-owned timestamps.
func (s *Store) Create(
	ctx context.Context,
	rule *eventrule.Rule,
) (*eventrule.Rule, error) {
	if rule == nil {
		return nil, fmt.Errorf("event rule is nil")
	}

	record, err := converterdao.EventRuleTo(rule)
	if err != nil {
		return nil, err
	}

	err = s.pg.DB.NewInsert().
		Model(record).
		Column("id", "name", "description", "enabled", "event_type", "policy").
		Returning("?Columns").
		Scan(ctx)
	if err != nil {
		return nil, err
	}

	return converterdao.EventRuleFrom(record)
}

// Delete locks the parent just as binding writers do, preventing check/delete races.
func (s *Store) Delete(ctx context.Context, id uuid.UUID) error {
	return s.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		var rule dbmodel.EventRule
		err := tx.NewSelect().Model(&rule).Where("er.id = ?", id).For("UPDATE").Scan(ctx)
		if errors.Is(err, sql.ErrNoRows) {
			return eventrule.ErrRuleNotFound
		}
		if err != nil {
			return err
		}
		bound, err := tx.NewSelect().Model((*dbmodel.EventRuleBinding)(nil)).Where("erb.rule_id = ?", id).Exists(ctx)
		if err != nil {
			return err
		}
		if bound {
			return fmt.Errorf("%w: policy still has bindings", eventrule.ErrBindingConflict)
		}
		_, err = tx.NewDelete().Model((*dbmodel.EventRule)(nil)).Where("id = ?", id).Exec(ctx)
		return err
	})
}

// SetEnabled returns the policy snapshot read or written by its transaction.
func (s *Store) SetEnabled(
	ctx context.Context,
	id uuid.UUID,
	enabled bool,
) (*eventrule.Rule, error) {
	var snapshot *eventrule.Rule
	err := s.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		var rule dbmodel.EventRule
		err := tx.NewSelect().
			Model(&rule).
			Where("er.id = ?", id).
			For("UPDATE").
			Scan(ctx)
		if errors.Is(err, sql.ErrNoRows) {
			return fmt.Errorf("%w: %s", eventrule.ErrRuleNotFound, id)
		}
		if err != nil {
			return err
		}
		if rule.Enabled != enabled {
			now, err := s.timestamp(ctx, tx)
			if err != nil {
				return err
			}
			err = tx.NewUpdate().
				Model(&rule).
				Set("enabled = ?", enabled).
				Set("updated_at = ?", now).
				Where("id = ?", id).
				Returning("?Columns").
				Scan(ctx)
			if err != nil {
				return err
			}
		}

		snapshot, err = converterdao.EventRuleFrom(&rule)
		return err
	})
	if err != nil {
		return nil, err
	}
	return snapshot, nil
}
