// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package Query

import (
	"errors"
)

const (
	OrderAscending  OrderDirection = "ASC"
	OrderDescending OrderDirection = "DESC"
)

type OrderDirection string

const DefaultPaginationLimit = 100

type Pagination struct {
	Offset int `json:"offset"`
	Limit  int `json:"limit"`
	Total  int `json:"total"`
}

// DefaultPagination returns a Pagination with offset 0 and the default limit.
func DefaultPagination() *Pagination {
	return &Pagination{Offset: 0, Limit: DefaultPaginationLimit}
}

func (p *Pagination) Validate() error {
	if p == nil {
		return nil
	}

	if p.Offset < 0 {
		return errors.New("offset must not be negative")
	}

	if p.Limit <= 0 {
		return errors.New("limit must be greater than 0")
	}

	return nil
}

type OrderBy struct {
	Column    string         `json:"column"`
	Direction OrderDirection `json:"direction"`
	// IsExpression marks a trusted SQL expression supplied by application code.
	IsExpression bool `json:"-"`
}

func (ob *OrderBy) Validate() error {
	if ob == nil {
		return nil
	}

	if ob.Column == "" {
		return errors.New("column is required")
	}

	if ob.Direction != OrderAscending && ob.Direction != OrderDescending {
		return errors.New("direction must be ASC or DESC")
	}

	return nil
}

func (ob *OrderBy) String() string {
	if ob.Direction == "" {
		return ob.Column
	}
	return ob.Column + " " + string(ob.Direction)
}
