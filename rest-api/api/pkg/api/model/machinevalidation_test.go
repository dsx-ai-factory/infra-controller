// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/stretchr/testify/assert"
)

func TestAPIMachineValidationTestCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIMachineValidationTestCreateRequest
		expectErr bool
	}{
		{
			desc:      "no error",
			obj:       APIMachineValidationTestCreateRequest{Name: "test-1", Command: "/bin/sh/test1", Args: "-p 12"},
			expectErr: false,
		},
		{
			desc:      "error no Name",
			obj:       APIMachineValidationTestCreateRequest{Command: "/bin/sh/test1", Args: "-p 12"},
			expectErr: true,
		},
		{
			desc:      "error no Command",
			obj:       APIMachineValidationTestCreateRequest{Name: "test-1", Args: "-p 12"},
			expectErr: true,
		},
		{
			desc:      "error no args",
			obj:       APIMachineValidationTestCreateRequest{Name: "test-1", Command: "/bin/sh/test1"},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPIMachineValidationExternalConfigCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIMachineValidationExternalConfigCreateRequest
		expectErr bool
	}{
		{
			desc:      "no error",
			obj:       APIMachineValidationExternalConfigCreateRequest{Name: "test-1", Description: cutil.GetPtr("test description"), Config: []byte{0, 1, 12}},
			expectErr: false,
		},
		{
			desc:      "no error with no description",
			obj:       APIMachineValidationExternalConfigCreateRequest{Name: "test-1", Config: []byte{0, 1, 12}},
			expectErr: false,
		},
		{
			desc:      "error no Name",
			obj:       APIMachineValidationExternalConfigCreateRequest{Config: []byte{0, 1, 12}},
			expectErr: true,
		},
		{
			desc:      "error no Config",
			obj:       APIMachineValidationExternalConfigCreateRequest{Name: "test-1"},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}
