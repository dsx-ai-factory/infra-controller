// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

// Int32PtrToIntPtr converts a *int32 to a *int
func Int32PtrToIntPtr(i *int32) *int {
	if i == nil {
		return nil
	}
	ret := int(*i)
	return &ret
}

// IntPtrToInt32Ptr converts a *int to a *int32
func IntPtrToInt32Ptr(i *int) *int32 {
	if i == nil {
		return nil
	}
	ret := int32(*i)
	return &ret
}

// StringPtr returns a pointer to the provided string
func StringPtr(s string) *string {
	return &s
}

// IntPtr returns a pointer to the provided int
func IntPtr(i int) *int {
	return &i
}
