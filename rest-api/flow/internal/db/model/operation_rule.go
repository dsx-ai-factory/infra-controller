// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

var defaultOperationRulePagination = dbquery.Pagination{
	Offset: 0,
	Limit:  100,
	Total:  0,
}

// OperationRule models the persisted operation rule metadata.
// Rules are templates that define how operations should be performed.
type OperationRule struct {
	bun.BaseModel `bun:"table:operation_rules,alias:or"`

	ID             uuid.UUID       `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Name           string          `bun:"name,notnull"`
	Description    sql.NullString  `bun:"description"`
	OperationType  string          `bun:"operation_type,notnull"`
	OperationCode  string          `bun:"operation_code,notnull"`
	RuleDefinition json.RawMessage `bun:"rule_definition,type:jsonb,notnull"`
	IsDefault      bool            `bun:"is_default,notnull,default:false"`
	CreatedAt      time.Time       `bun:"created_at,notnull,default:current_timestamp"`
	UpdatedAt      time.Time       `bun:"updated_at,notnull,default:current_timestamp"`
}

// Create inserts the operation rule record into the backing store.
func (r *OperationRule) Create(ctx context.Context, idb bun.IDB) error {
	r.CreatedAt = time.Now().UTC()
	r.UpdatedAt = r.CreatedAt
	_, err := idb.NewInsert().Model(r).Exec(ctx)
	return err
}

// Update updates specific fields of the operation rule.
func (r *OperationRule) Update(ctx context.Context, idb bun.IDB, columns ...string) error {
	if len(columns) == 0 {
		return fmt.Errorf("no columns specified for update")
	}

	r.UpdatedAt = time.Now().UTC()
	columns = append(columns, "updated_at")

	_, err := idb.NewUpdate().
		Model(r).
		Column(columns...).
		Where("id = ?", r.ID).
		Exec(ctx)

	return err
}

// Delete deletes the operation rule from the backing store.
func (r *OperationRule) Delete(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewDelete().
		Model(r).
		Where("id = ?", r.ID).
		Exec(ctx)
	return err
}

// GetOperationRule retrieves an operation rule by its UUID.
func GetOperationRule(ctx context.Context, idb bun.IDB, id uuid.UUID) (*OperationRule, error) {
	if id == uuid.Nil {
		return nil, fmt.Errorf("operation rule UUID is required")
	}

	var rule OperationRule
	err := idb.NewSelect().
		Model(&rule).
		Where("id = ?", id).
		Scan(ctx)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, nil
		}
		return nil, err
	}

	return &rule, nil
}

// OperationRuleListOptions defines filter options for listing operation rules.
// ListOperationRules returns all operation rules matching the given criteria with pagination.
func ListOperationRules(
	ctx context.Context,
	idb bun.IDB,
	options *taskcommon.OperationRuleListOptions,
	pagination *dbquery.Pagination,
) ([]OperationRule, int32, error) {
	var rules []OperationRule
	conf := &dbquery.Config{
		IDB:   idb,
		Model: &rules,
	}

	if pagination != nil {
		conf.Pagination = pagination
	} else {
		conf.Pagination = &defaultOperationRulePagination
	}

	if filterable := operationRuleListOptionsToFilterable(options); filterable != nil {
		conf.Filterables = []dbquery.Filterable{filterable}
	}

	// Order newest first, with the UUID as a stable tie-breaker.
	conf.DefaultOrderBy = []dbquery.OrderBy{
		{Column: "created_at", Direction: dbquery.OrderDescending},
		{Column: "id", Direction: dbquery.OrderDescending},
	}

	q, err := dbquery.New(ctx, conf)
	if err != nil {
		return nil, 0, err
	}

	if err := q.Scan(ctx); err != nil {
		return nil, 0, err
	}

	return rules, int32(q.TotalCount()), nil
}

// operationRuleListOptionsToFilterable converts OperationRuleListOptions to dbquery.Filterable
func operationRuleListOptionsToFilterable(
	options *taskcommon.OperationRuleListOptions,
) dbquery.Filterable {
	if options == nil {
		return nil
	}

	filters := make([]dbquery.Filter, 0, 2)

	if options.OperationType != taskcommon.TaskTypeUnknown {
		filters = append(filters, dbquery.Filter{
			Column:   "operation_type",
			Operator: dbquery.OperatorEqual,
			Value:    string(options.OperationType),
		})
	}

	if options.IsDefault != nil {
		filters = append(filters, dbquery.Filter{
			Column:   "is_default",
			Operator: dbquery.OperatorEqual,
			Value:    *options.IsDefault,
		})
	}

	if len(filters) == 0 {
		return nil
	}

	return &dbquery.FilterGroup{
		Connector: dbquery.ConnectorAND,
		Filters:   filters,
	}
}

// RackRuleAssociation models the association between a rack and an operation rule.
// It defines which specific rule a rack should use for a given operation type.
type RackRuleAssociation struct {
	bun.BaseModel `bun:"table:rack_rule_associations,alias:rra"`

	RackID        uuid.UUID `bun:"rack_id,pk,type:uuid,notnull"`
	OperationType string    `bun:"operation_type,pk,type:varchar(64),notnull"`
	OperationCode string    `bun:"operation_code,pk,type:varchar(64),notnull"`
	RuleID        uuid.UUID `bun:"rule_id,type:uuid,notnull"`
	CreatedAt     time.Time `bun:"created_at,notnull,default:current_timestamp"`
	UpdatedAt     time.Time `bun:"updated_at,notnull,default:current_timestamp"`
}

// Create inserts the rack rule association into the backing store.
func (a *RackRuleAssociation) Create(ctx context.Context, idb bun.IDB) error {
	a.CreatedAt = time.Now().UTC()
	a.UpdatedAt = a.CreatedAt
	_, err := idb.NewInsert().Model(a).Exec(ctx)
	return err
}

// Delete removes the rack rule association from the backing store.
func (a *RackRuleAssociation) Delete(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewDelete().Model(a).WherePK().Exec(ctx)
	return err
}

// GetRackRuleAssociation retrieves the rule association for a rack, operation type, and operation code.
func GetRackRuleAssociation(
	ctx context.Context,
	idb bun.IDB,
	rackID uuid.UUID,
	opType taskcommon.TaskType,
	operationCode string,
) (*RackRuleAssociation, error) {
	var assoc RackRuleAssociation
	err := idb.NewSelect().
		Model(&assoc).
		Where("rack_id = ?", rackID).
		Where("operation_type = ?", string(opType)).
		Where("operation_code = ?", operationCode).
		Scan(ctx)

	if err != nil {
		return nil, err
	}

	return &assoc, nil
}

// ListRackRuleAssociations retrieves all rule associations for a rack.
func ListRackRuleAssociations(
	ctx context.Context,
	idb bun.IDB,
	rackID uuid.UUID,
) ([]RackRuleAssociation, error) {
	var assocs []RackRuleAssociation
	err := idb.NewSelect().
		Model(&assocs).
		Where("rack_id = ?", rackID).
		Scan(ctx)

	if err != nil {
		return nil, err
	}

	return assocs, nil
}
