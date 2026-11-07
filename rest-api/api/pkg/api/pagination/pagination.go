// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pagination

import (
	"fmt"
	"math"
	"regexp"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

const (
	// MaxPageSize is the maximum page size allowed
	MaxPageSize = 100

	// MaxPageNumber is the maximum page number allowed. It bounds PageNumber so
	// that the largest offset Validate can derive, (MaxPageNumber-1) *
	// MaxPageSize, still fits the int32 every consumer narrows Offset to. Past
	// that the narrowed offset wraps to an unrelated or negative value and the
	// response reports a page it does not contain.
	MaxPageNumber = math.MaxInt32 / MaxPageSize

	// ResponseHeaderName describes the header name for the pagination response
	ResponseHeaderName = "X-Pagination"
)

var (
	// OrderByRegex is the regex for the orderBy field
	OrderByRegex = fmt.Sprintf("^[A-Z0-9_]+_(%v|%v)$", cdbp.OrderAscending, cdbp.OrderDescending)
)

// PageRequest captures and validates pagination data
type PageRequest struct {
	// Name is the name of the Allocation
	PageNumber *int `query:"pageNumber"`
	// Description is the description of the Allocation
	PageSize *int `query:"pageSize"`
	// OrderByStr is the order by field
	OrderByStr *string `query:"orderBy"`

	// Offset is the offset of the pagination
	Offset *int
	// Limit is the limit of the pagination
	Limit *int
	// OrderBy is the processed order by field
	OrderBy *cdbp.OrderBy
}

// Validate ensure the values passed in request are acceptable
func (pr *PageRequest) Validate(orderByFields []string) error {
	err := validation.ValidateStruct(pr,
		validation.Field(&pr.PageNumber,
			validation.Min(1).Error("must be greater than 0"),
			validation.Max(MaxPageNumber).Error(fmt.Sprintf("must be less than or equal to: %v", MaxPageNumber)),
		),
		validation.Field(&pr.PageSize,
			validation.Min(1).Error("must be greater than 0"),
			validation.Max(MaxPageSize).Error(fmt.Sprintf("must be less than or equal to: %v", MaxPageSize)),
		),
		validation.Field(&pr.OrderByStr,
			validation.Match(regexp.MustCompile(OrderByRegex)).Error(fmt.Sprintf("must be in the format of field_%v or field_%v", cdbp.OrderAscending, cdbp.OrderDescending)),
		),
	)

	if err != nil {
		return err
	}

	if pr.PageNumber == nil || *pr.PageNumber == 0 {
		pr.PageNumber = cutil.GetPtr(1)
	}

	if pr.PageSize == nil || *pr.PageSize == 0 {
		pr.PageSize = cutil.GetPtr(cdbp.DefaultLimit)
	}

	offset := (*pr.PageNumber - 1) * *pr.PageSize

	pr.Offset = cutil.GetPtr(offset)
	pr.Limit = pr.PageSize

	if pr.OrderByStr != nil {
		if orderByFields == nil {
			return fmt.Errorf("orderBy fields must be provided as an argument")
		}

		orderBy := &cdbp.OrderBy{}
		err = orderBy.FromAPIRequest(*pr.OrderByStr)
		if err != nil {
			return err
		}

		if !cdb.IsStrInSlice(orderBy.Field, orderByFields) {
			return fmt.Errorf("orderBy field %v is not valid", orderBy.Field)
		}

		pr.OrderBy = orderBy
	}

	return nil
}

// PageResponse is the response for a paginated request
type PageResponse struct {
	// PageNumber is the page number
	PageNumber int `json:"pageNumber"`
	// PageSize is the page size
	PageSize int `json:"pageSize"`
	// Total is the total number of items
	Total int `json:"total"`
	// OrderBy is the order by field
	OrderBy *string `json:"orderBy"`
}

// NewPageResponse creates a new PageResponse
func NewPageResponse(pageNumber int, pageSize int, total int, orderBy *string) *PageResponse {
	return &PageResponse{
		PageNumber: pageNumber,
		PageSize:   pageSize,
		Total:      total,
		OrderBy:    orderBy,
	}
}

func (pr *PageRequest) ConvertToDB() cdbp.PageInput {
	if pr == nil {
		return cdbp.PageInput{}
	}
	return cdbp.PageInput{
		Offset:  pr.Offset,
		Limit:   pr.Limit,
		OrderBy: pr.OrderBy,
	}
}
