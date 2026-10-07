// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"strconv"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/labstack/echo/v4"
)

// QueryOverride provides values that override query params when delegating from
// path-scoped endpoints (e.g. instance/{instanceId}/interface, instance/{instanceId}/nvlink-interface) to more general endpoints.
// When set, error messages in general endpoints will be modulated
type QueryOverride struct {
	InstanceIDs   []string
	ValueFromPath bool
}

// GetSearchQuery returns a trimmed search query or nil when the query is blank.
func GetSearchQuery(c echo.Context) *string {
	rawQuery := c.QueryParams().Get("query")

	searchQuery, ok := cdb.TrimSearchQuery(rawQuery)
	if !ok {
		return nil
	}

	return &searchQuery
}

// ParseOptionalBoolQueryParam parses an optional boolean query parameter,
// returning nil when the parameter is absent so callers can distinguish
// "not provided" from an explicit false. It returns an error when the parameter
// is present but is not a valid boolean. Use this for tri-state filters where
// an absent parameter must mean "do not filter" rather than "filter on false".
func ParseOptionalBoolQueryParam(c echo.Context, name string) (*bool, error) {
	raw := c.QueryParam(name)
	if raw == "" {
		return nil, nil
	}

	v, err := strconv.ParseBool(raw)
	if err != nil {
		return nil, err
	}

	return &v, nil
}
