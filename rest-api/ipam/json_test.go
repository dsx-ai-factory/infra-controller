/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestPrefixes_JSON(t *testing.T) {

	p1 := Prefix{
		Cidr:                   "192.168.0.0/24",
		ParentCidr:             "192.168.0.0/20",
		isParent:               false,
		availableChildPrefixes: map[string]bool{},
		childPrefixLength:      0,
		ips:                    map[string]bool{"192.168.0.1": true, "192.168.0.2": true},
		version:                0,
	}

	p2 := Prefix{
		Cidr:                   "172.17.0.0/24",
		ParentCidr:             "172.17.0.0/20",
		isParent:               false,
		availableChildPrefixes: map[string]bool{},
		childPrefixLength:      0,
		ips:                    map[string]bool{"172.17.0.1": true, "172.17.0.2": true},
		version:                0,
	}

	p1j, err := p1.toJSON()
	require.NoError(t, err)
	require.NotNil(t, p1j)
	p2j, err := p2.toJSON()
	require.NoError(t, err)
	require.NotNil(t, p1j)

	p1reverse, err := fromJSON(p1j)
	require.NoError(t, err)
	require.NotNil(t, p1reverse)
	require.Equal(t, p1, p1reverse)

	p2reverse, err := fromJSON(p2j)
	require.NoError(t, err)
	require.NotNil(t, p2reverse)
	require.Equal(t, p2, p2reverse)

	ps1 := Prefixes{p1, p2}

	ps1j, err := ps1.toJSON()
	require.NoError(t, err)
	require.NotNil(t, ps1j)

	ps1reverse, err := fromJSONs(ps1j)
	require.NoError(t, err)
	require.NotNil(t, ps1reverse)
	require.Equal(t, ps1, ps1reverse)

}
