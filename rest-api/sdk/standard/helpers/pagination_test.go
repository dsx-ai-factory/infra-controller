// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package helpers

import (
	"context"
	"net/http"
	"net/textproto"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestGetPaginationResponse(t *testing.T) {
	canonicalHeader := textproto.CanonicalMIMEHeaderKey(PaginationHeader)

	t.Run("valid header is parsed correctly", func(t *testing.T) {
		orderBy := "name"
		response := &http.Response{
			Header: http.Header{
				canonicalHeader: []string{`{"pageNumber":2,"pageSize":50,"total":120,"orderBy":"name"}`},
			},
		}

		pagination, err := GetPaginationResponse(context.Background(), response)
		require.NoError(t, err)
		assert.Equal(t, 2, pagination.PageNumber)
		assert.Equal(t, 50, pagination.PageSize)
		assert.Equal(t, 120, pagination.Total)
		assert.Equal(t, &orderBy, pagination.OrderBy)
	})

	t.Run("null orderBy field is nil", func(t *testing.T) {
		response := &http.Response{
			Header: http.Header{
				canonicalHeader: []string{`{"pageNumber":1,"pageSize":100,"total":46,"orderBy":null}`},
			},
		}

		pagination, err := GetPaginationResponse(context.Background(), response)
		require.NoError(t, err)
		assert.Equal(t, 1, pagination.PageNumber)
		assert.Equal(t, 100, pagination.PageSize)
		assert.Equal(t, 46, pagination.Total)
		assert.Nil(t, pagination.OrderBy)
	})

	t.Run("missing header returns error", func(t *testing.T) {
		response := &http.Response{
			Header: http.Header{},
		}

		pagination, err := GetPaginationResponse(context.Background(), response)
		assert.ErrorContains(t, err, "pagination header not found in response")
		assert.Nil(t, pagination)
	})

	t.Run("nil response returns error", func(t *testing.T) {
		pagination, err := GetPaginationResponse(context.Background(), nil)
		assert.ErrorContains(t, err, "response is nil")
		assert.Nil(t, pagination)
	})

	t.Run("malformed header returns error", func(t *testing.T) {
		response := &http.Response{
			Header: http.Header{
				canonicalHeader: []string{`not-valid-json`},
			},
		}

		pagination, err := GetPaginationResponse(context.Background(), response)
		assert.ErrorContains(t, err, "failed to unmarshal pagination header")
		assert.Nil(t, pagination)
	})
}
