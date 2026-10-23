// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
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

func TestNewAPIMachineValidationResult(t *testing.T) {
	proto := &corev1.MachineValidationResult{
		Name:   "DcgmFullLong",
		StdOut: "Diagnostic completed successfully",
		StdErr: "fan speed warning",
	}

	tests := []struct {
		desc          string
		includeOutput bool
		wantStdOut    string
		wantStdErr    string
	}{
		{desc: "output included", includeOutput: true, wantStdOut: proto.StdOut, wantStdErr: proto.StdErr},
		{desc: "output left empty", includeOutput: false},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			result := NewAPIMachineValidationResult(proto, tc.includeOutput)
			assert.Equal(t, proto.Name, result.Name)
			assert.Equal(t, tc.wantStdOut, result.StdOut)
			assert.Equal(t, tc.wantStdErr, result.StdErr)
		})
	}
}
