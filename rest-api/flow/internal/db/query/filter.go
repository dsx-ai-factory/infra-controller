// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package Query

import (
	"errors"

	"github.com/uptrace/bun"
)

const (
	OperatorIn                 Operator = "IN"
	OperatorNotIn              Operator = "NOT IN"
	OperatorLike               Operator = "LIKE"
	OperatorNotLike            Operator = "NOT LIKE"
	OperatorEqual              Operator = "="
	OperatorGreaterThan        Operator = ">"
	OperatorLessThan           Operator = "<"
	OperatorGreaterThanOrEqual Operator = ">="
	OperatorLessThanOrEqual    Operator = "<="
	OperatorNotEqual           Operator = "<>"
	OperatorJSONBContains      Operator = "@>"

	ConnectorAND Connector = " AND "
	ConnectorOR  Connector = " OR "
)

type Operator string
type Connector string

// Filterable is an interface for types that can be applied to a Bun query
type Filterable interface {
	ApplyTo(q *bun.SelectQuery) *bun.SelectQuery
}

type Filter struct {
	Column   string
	Operator Operator
	Value    any
}

type FilterGroup struct {
	Filters   []Filter
	Connector Connector
}

func (f *FilterGroup) ApplyTo(q *bun.SelectQuery) *bun.SelectQuery {
	if len(f.Filters) <= 1 {
		for _, filter := range f.Filters {
			q = filter.ApplyTo(q)
		}

		return q
	}

	for _, filter := range f.Filters {
		q = filter.applyToWithConnector(q, f.Connector)
	}

	return q
}

func (f *Filter) ApplyTo(q *bun.SelectQuery) *bun.SelectQuery {
	return f.applyToWithConnector(q, ConnectorAND)
}

func (f *Filter) applyToWithConnector(
	q *bun.SelectQuery,
	connector Connector,
) *bun.SelectQuery {
	var qs string
	var args []any

	switch f.Operator {
	case OperatorIn, OperatorNotIn:
		qs = f.Column + " " + string(f.Operator) + " (?)"
		args = []any{bun.In(f.Value)}
	case OperatorJSONBContains:
		// JSONB containment: column @> 'json_value'::jsonb
		qs = f.Column + " @> ?::jsonb"
		args = []any{f.Value}
	default:
		qs = f.Column + " " + string(f.Operator) + " ?"
		args = []any{f.Value}
	}

	if connector == ConnectorOR {
		return q.WhereOr(qs, args...)
	}

	return q.Where(qs, args...)
}

func (f *Filter) Validate() error {
	if f.Column == "" {
		return errors.New("column is required")
	}

	if f.Value == nil {
		return errors.New("value is required")
	}

	return nil
}

type StringQueryInfo struct {
	Patterns   []string
	IsWildcard bool
	UseOR      bool
}

func (info *StringQueryInfo) ToFilterable(col string) Filterable {
	if len(info.Patterns) == 0 {
		return nil
	}

	// If only one pattern, return a single Filter
	if len(info.Patterns) == 1 {
		if info.IsWildcard {
			return &Filter{
				Column:   col,
				Operator: OperatorLike,
				Value:    wildCardString(info.Patterns[0]),
			}
		}

		return &Filter{
			Column:   col,
			Operator: OperatorEqual,
			Value:    info.Patterns[0],
		}
	}

	if info.IsWildcard {
		// For wildcards with multiple patterns, create a FilterGroup with
		// LIKE conditions
		filters := make([]Filter, 0, len(info.Patterns))
		for _, p := range info.Patterns {
			filters = append(filters, Filter{
				Column:   col,
				Operator: OperatorLike,
				Value:    wildCardString(p),
			})
		}

		return &FilterGroup{
			Filters:   filters,
			Connector: connector(info.UseOR),
		}
	}

	// For non-wildcard with multiple patterns, create a filter with IN
	// operator
	return &Filter{
		Column:   col,
		Operator: OperatorIn,
		Value:    info.Patterns,
	}
}

func wildCardString(s string) string {
	if len(s) > 0 && s[0] != '%' && s[len(s)-1] != '%' {
		return "%" + s + "%"
	}

	return s
}

func connector(useOR bool) Connector {
	if useOR {
		return ConnectorOR
	}

	return ConnectorAND
}
