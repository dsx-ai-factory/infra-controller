/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package vendor

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestCodeToVendor(t *testing.T) {
	testCases := map[string]struct {
		inCode      VendorCode
		wantCode    VendorCode
		wantName    string
		wantSupport bool
	}{
		"unsupported code -> Unsupported vendor": {
			inCode:      VendorCodeUnsupported,
			wantCode:    VendorCodeUnsupported,
			wantName:    "Unsupported",
			wantSupport: false,
		},
		"nvidia code -> NVIDIA vendor": {
			inCode:      VendorCodeNVIDIA,
			wantCode:    VendorCodeNVIDIA,
			wantName:    VendorNVIDIA,
			wantSupport: true,
		},
		"max sentinel -> Unsupported vendor": {
			inCode:      VendorCodeMax,
			wantCode:    VendorCodeMax,
			wantName:    "Unsupported",
			wantSupport: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			v := CodeToVendor(tc.inCode)
			assert.Equal(t, tc.wantCode, v.Code)
			assert.Equal(t, tc.wantName, v.Name)

			err := v.IsSupported()
			if tc.wantSupport {
				assert.NoError(t, err)
			} else {
				assert.Error(t, err)
			}
		})
	}
}

func TestStringToVendor(t *testing.T) {
	testCases := map[string]struct {
		inName      string
		wantCode    VendorCode
		wantName    string
		wantString  string
		wantSupport bool
	}{
		"NVIDIA name -> NVIDIA code": {
			inName:      VendorNVIDIA,
			wantCode:    VendorCodeNVIDIA,
			wantName:    VendorNVIDIA,
			wantString:  VendorNVIDIA,
			wantSupport: true,
		},
		"unknown name -> Unsupported code": {
			inName:   "FooCorp",
			wantCode: VendorCodeUnsupported,
			wantName: "FooCorp",
			// String should report unsupported error; exact message includes code and name
			wantString:  "unsupported vendor: FooCorp (0)",
			wantSupport: false,
		},
		"empty name -> Unsupported code": {
			inName:      "",
			wantCode:    VendorCodeUnsupported,
			wantName:    "",
			wantString:  "unsupported vendor:  (0)",
			wantSupport: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			v := StringToVendor(tc.inName)
			assert.Equal(t, tc.wantCode, v.Code)
			assert.Equal(t, tc.wantName, v.Name)

			err := v.IsSupported()
			if tc.wantSupport {
				assert.NoError(t, err)
			} else {
				assert.Error(t, err)
			}

			// Verify String() behavior (uses IsSupported)
			assert.Equal(t, tc.wantString, v.String())
		})
	}
}

func TestVendorIsSupported(t *testing.T) {
	testCases := map[string]struct {
		in            Vendor
		wantSupported bool
	}{
		"unsupported code": {
			in:            CodeToVendor(VendorCodeUnsupported),
			wantSupported: false,
		},
		"nvidia code": {
			in:            CodeToVendor(VendorCodeNVIDIA),
			wantSupported: true,
		},
		"max sentinel code": {
			in:            CodeToVendor(VendorCodeMax),
			wantSupported: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			err := tc.in.IsSupported()
			if tc.wantSupported {
				assert.NoError(t, err)
			} else {
				assert.Error(t, err)
			}
		})
	}
}

func TestVendorString(t *testing.T) {
	testCases := map[string]struct {
		in   Vendor
		want string
	}{
		"nvidia string": {
			in:   CodeToVendor(VendorCodeNVIDIA),
			want: VendorNVIDIA,
		},
		"unsupported string (Unsupported name)": {
			in:   CodeToVendor(VendorCodeUnsupported),
			want: "unsupported vendor: Unsupported (0)",
		},
		"unsupported string (Max sentinel)": {
			in:   CodeToVendor(VendorCodeMax),
			want: "unsupported vendor: Unsupported (2)",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := tc.in.String()
			assert.Equal(t, tc.want, got)
		})
	}
}

func TestRoundTrip_NameAndCode(t *testing.T) {
	testCases := map[string]struct {
		code VendorCode
	}{
		"round-trip NVIDIA":       {code: VendorCodeNVIDIA},
		"round-trip Unsupported":  {code: VendorCodeUnsupported},
		"round-trip Max sentinel": {code: VendorCodeMax},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			// CodeToVendor -> StringToVendor(Name)
			v := CodeToVendor(tc.code)
			v2 := StringToVendor(v.Name)

			// For NVIDIA, code should round-trip. For unsupported names, StringToVendor maps to Unsupported.
			if tc.code == VendorCodeNVIDIA {
				assert.Equal(t, VendorCodeNVIDIA, v2.Code)
				assert.Equal(t, VendorNVIDIA, v2.Name)
			} else {
				assert.Equal(t, VendorCodeUnsupported, v2.Code)
				assert.Equal(t, v.Name, v2.Name)
			}
		})
	}
}
