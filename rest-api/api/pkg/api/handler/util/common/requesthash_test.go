// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestRequestSHA256(t *testing.T) {
	for _, test := range []struct {
		name    string
		request any
		want    string
		bad     bool
	}{
		{name: "null", want: "74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b"},
		{name: "stable map order", request: map[string]int{"b": 2, "a": 1}, want: "43258cff783fe7036d8a43033f830adfc60ec037382473548ac742b888292777"},
		{name: "Rack request", request: map[string]string{"id": "rack-17"}, want: "607d650753361b5810fe93eaadb7b53e644c19d17907ec8dc9f78eba0c45ce52"},
		{name: "different Rack request", request: map[string]string{"id": "rack-18"}, want: "15bfa163fc98d6673cc7e67cd1d0349aad98b958c8834630344458ed71651a97"},
		{name: "serialization failure", request: make(chan struct{}), bad: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			actual, err := RequestSHA256(test.request)
			if test.bad {
				var cause *json.UnsupportedTypeError
				require.ErrorAs(t, err, &cause)
				require.Empty(t, actual)
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.want, actual)
			repeated, err := RequestSHA256(test.request)
			require.NoError(t, err)
			require.Equal(t, actual, repeated)
			require.Equal(t, actual[:12], RequestHash(test.request))
		})
	}
}
