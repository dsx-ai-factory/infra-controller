// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probeapi

import (
	"encoding/json"
	"fmt"
	"net/url"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

// Query is the filter as the query parameters of GET /v1/count and
// /v1/units: one parameter per set field, named as in the JSON body. It
// fails on a value Encode refuses.
func (f Filter) Query() (url.Values, error) {
	body, err := jsonx.Encode(f)
	if err != nil {
		return nil, err
	}
	var fields map[string]any
	if err := json.Unmarshal(body, &fields); err != nil {
		return nil, err
	}
	q := url.Values{}
	for name, v := range fields {
		q.Set(name, fmt.Sprint(v)) // every filter field is a string or a bool
	}
	return q, nil
}
