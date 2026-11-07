// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package helpers

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
)

// PaginationHeader is the HTTP header key for pagination metadata.
const PaginationHeader = "x-pagination"

// PaginationResponse is the response contained in the x-pagination header of http response.
type PaginationResponse struct {
	PageNumber int     `json:"pageNumber"`
	PageSize   int     `json:"pageSize"`
	Total      int     `json:"total"`
	OrderBy    *string `json:"orderBy,omitempty"`
}

// GetPaginationResponse extracts the pagination response from the JSON contained in the x-pagination header.
func GetPaginationResponse(ctx context.Context, response *http.Response) (*PaginationResponse, error) {
	if response == nil {
		return nil, fmt.Errorf("cannot extract pagination header: response is nil")
	}
	pagination := &PaginationResponse{}
	paginationHeader := response.Header.Get(PaginationHeader)
	if paginationHeader == "" {
		return nil, fmt.Errorf("pagination header not found in response")
	}
	err := json.Unmarshal([]byte(paginationHeader), pagination)
	if err != nil {
		return nil, fmt.Errorf("failed to unmarshal pagination header: %w", err)
	}
	return pagination, nil
}
