// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package paginator

import (
	"context"
	"errors"
	"regexp"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/uptrace/bun"
)

const (
	// DefaultOffset is the default offset for pagination
	DefaultOffset = 0
	// DefaultLimit is the default limit for pagination
	DefaultLimit = 20
	// TotalLimit is the limit for getting all objects from pagination
	TotalLimit = -1

	// OrderAscending specifies the ascending order for pagination
	OrderAscending = "ASC"
	// OrderDescending specifies the descending order for pagination
	OrderDescending = "DESC"
)

var (
	// ErrOrderFieldsRequired is returned when order fields are not provided
	ErrOrderFieldsRequired = errors.New("order fields are not provided")
	// ErrInvalidOrderField is returned when order field is not valid
	ErrInvalidOrderField = errors.New("invalid order field")
	// ErrInvalidOrder is returned when order is not valid
	ErrInvalidOrder = errors.New("invalid order, must be ASC or DESC")
	// ErrInvalidOrderBy is raised when an invalid order by field is provided
	ErrInvalidOrderBy = errors.New("invalid order by field")

	// ErrQueryTotalCount is raised when paginating a query and the count fails
	ErrQueryTotalCount = errors.New("error counting query results")
)

// OrderBy defines the order for results returned
type OrderBy struct {
	Field string
	Order string
}

var apiOrderByPattern = regexp.MustCompile(`^[A-Z0-9_]+_(ASC|DESC)$`)

// FromAPIRequest parses a FIELD_ASC or FIELD_DESC value, preserving underscores
// in field names. Allowed fields are validated separately by the caller.
// Invalid input leaves the receiver unchanged.
func (ob *OrderBy) FromAPIRequest(value string) error {
	if !apiOrderByPattern.MatchString(value) {
		return ErrInvalidOrderBy
	}
	separator := strings.LastIndexByte(value, '_')
	ob.Field = strings.ToLower(value[:separator])
	ob.Order = value[separator+1:]
	return nil
}

// ToAPIRequest formats an order as FIELD_ASC or FIELD_DESC. The receiver must
// contain a valid field and direction.
func (ob OrderBy) ToAPIRequest() string {
	return strings.ToUpper(ob.Field) + "_" + ob.Order
}

type PageInput struct {
	Offset  *int
	Limit   *int
	OrderBy *OrderBy
}

// Paginator is a helper struct for paginating query results
type Paginator struct {
	Total  int
	Offset int
	Limit  int
	Query  *bun.SelectQuery
}

// NewPaginator creates and returns a new Paginator
func NewPaginator(ctx context.Context, query *bun.SelectQuery, offset, limit *int, orderBy *OrderBy, orderFields []string) (*Paginator, error) {
	var multiOrderBy []*OrderBy
	if orderBy != nil {
		multiOrderBy = append(multiOrderBy, orderBy)
	}
	return NewPaginatorMultiOrderBy(ctx, query, offset, limit, multiOrderBy, orderFields)
}

// NewPaginatorMultiOrderBy same as NewPaginator but allows multiple OrderBy
func NewPaginatorMultiOrderBy(ctx context.Context, query *bun.SelectQuery, offset, limit *int, multiOrderBy []*OrderBy, orderFields []string) (*Paginator, error) {

	paginator := &Paginator{
		Offset: DefaultOffset,
		Limit:  DefaultLimit,
		Query:  query,
	}

	// Check ordering
	if len(multiOrderBy) > 0 && len(orderFields) == 0 {
		return nil, ErrOrderFieldsRequired
	}
	for _, ob := range multiOrderBy {
		if !db.IsStrInSlice(ob.Field, orderFields) {
			return nil, ErrInvalidOrderField
		}

		if ob.Order != OrderAscending && ob.Order != OrderDescending {
			return nil, ErrInvalidOrder
		}

		orderByParam := ob.Field + " " + ob.Order
		paginator.Query = query.Order(orderByParam)
	}

	total, err := query.Count(ctx)
	if err != nil {
		// Why do we not wrap err vs shadowing the original error?
		return nil, ErrQueryTotalCount
	}

	paginator.Total = total

	if offset != nil {
		paginator.Offset = *offset
		if *offset < 0 {
			paginator.Offset = 0
		}
	}

	if limit != nil {
		paginator.Limit = *limit
		if *limit == TotalLimit {
			paginator.Limit = total
		} else if *limit < 0 {
			paginator.Limit = DefaultLimit
		}
	}

	return paginator, nil
}

func NewDefaultOrderBy(defaultField string) *OrderBy {
	return &OrderBy{
		Field: defaultField,
		Order: OrderAscending,
	}
}
