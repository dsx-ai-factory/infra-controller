// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package Query

import (
	"context"
	"errors"
	"fmt"
	"slices"

	"github.com/uptrace/bun"
)

type Query struct {
	pagination Pagination
	columns    []string
	query      *bun.SelectQuery
}

type Config struct {
	IDB            bun.IDB
	Model          any
	Pagination     *Pagination
	DefaultOrderBy []OrderBy
	Filterables    []Filterable
	Columns        []string
	Relations      []string
}

func (c *Config) Validate() error {
	if c == nil {
		return errors.New("config is required")
	}

	if c.IDB == nil {
		return errors.New("idb is required")
	}

	if c.Model == nil {
		return errors.New("model is required")
	}

	if slices.Contains(c.Columns, "") {
		return errors.New("columns cannot be empty")
	}

	if slices.Contains(c.Relations, "") {
		return errors.New("relations cannot be empty")
	}

	for _, ob := range c.DefaultOrderBy {
		if err := ob.Validate(); err != nil {
			return fmt.Errorf("invalid default order by: %w", err)
		}
	}

	for _, f := range c.Filterables {
		// Validate if it's a Filter type
		if filter, ok := f.(*Filter); ok {
			if err := filter.Validate(); err != nil {
				return err
			}
		}
	}

	if err := c.Pagination.Validate(); err != nil {
		return err
	}

	return nil
}

// New builds a query with the configured filters, ordering, and pagination,
// counting matching rows when the pagination total is not positive.
func New(ctx context.Context, conf *Config) (*Query, error) {
	if err := conf.Validate(); err != nil {
		return nil, err
	}

	q := conf.IDB.NewSelect().Model(conf.Model)

	if len(conf.Columns) > 0 {
		q = q.Column(conf.Columns...)
	}

	for _, rel := range conf.Relations {
		q = q.Relation(rel)
	}

	for _, filterable := range conf.Filterables {
		q = filterable.ApplyTo(q)
	}

	for _, orderBy := range conf.DefaultOrderBy {
		if orderBy.IsExpression {
			q = q.OrderExpr(orderBy.String())
		} else {
			q = q.Order(orderBy.String())
		}
	}

	if conf.Pagination != nil {
		q = q.Offset(conf.Pagination.Offset).Limit(conf.Pagination.Limit)
	}

	query := &Query{
		pagination: *conf.Pagination,
		query:      q,
		columns:    append([]string{}, conf.Columns...),
	}

	if conf.Pagination != nil && conf.Pagination.Total <= 0 {
		// Need to query for the total count and record it.
		total, err := q.Count(ctx)
		if err != nil {
			return nil, fmt.Errorf("failed to count total: %w", err)
		}

		query.pagination.Total = total
	}

	return query, nil
}

func (q *Query) Scan(ctx context.Context) error {
	return q.query.Scan(ctx)
}

func (q *Query) TotalCount() int {
	return q.pagination.Total
}
