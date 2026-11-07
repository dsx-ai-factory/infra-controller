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
	"fmt"
)

// Vendor represents a BMC vendor with a numeric code and a display name.
type Vendor struct {
	Code VendorCode `json:"code"`
	Name string     `json:"name"`
}

// VendorCode enumerates known vendors; VendorCodeMax is sentinel for iteration bounds.
type VendorCode int

const (
	VendorCodeUnsupported VendorCode = iota
	VendorCodeNVIDIA
	VendorCodeMax
)

const (
	VendorNVIDIA = "NVIDIA"
)

// CodeToVendor maps a vendor code to a Vendor struct.
func CodeToVendor(code VendorCode) Vendor {
	var v string

	switch code {
	case VendorCodeNVIDIA:
		v = VendorNVIDIA
	default:
		v = "Unsupported"
	}

	return Vendor{code, v}
}

// StringToVendor maps a vendor display name to a Vendor struct.
func StringToVendor(v string) Vendor {
	var code VendorCode

	switch v {
	case VendorNVIDIA:
		code = VendorCodeNVIDIA
	default:
		code = VendorCodeUnsupported
	}

	return Vendor{code, v}
}

// String returns the vendor's display name or an error string if unsupported.
func (v Vendor) String() string {
	if err := v.IsSupported(); err != nil {
		return err.Error()
	}

	return v.Name
}

// IsSupported reports whether the vendor code is within supported range.
func (v Vendor) IsSupported() error {
	if v.Code > VendorCodeUnsupported && v.Code < VendorCodeMax {
		return nil
	}

	return fmt.Errorf("unsupported vendor: %s (%v)", v.Name, v.Code)
}
