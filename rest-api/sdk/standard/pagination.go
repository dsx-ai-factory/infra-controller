// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard/helpers"
)

const PaginationHeader = helpers.PaginationHeader

// PaginationResponse is the response contained in the x-pagination header of http response.
type PaginationResponse = helpers.PaginationResponse

// GetPaginationResponse extracts the pagination response from the JSON contained in the x-pagination header.
func GetPaginationResponse(ctx context.Context, response *http.Response) (*PaginationResponse, error) {
	return helpers.GetPaginationResponse(ctx, response)
}
