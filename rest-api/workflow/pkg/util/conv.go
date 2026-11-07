// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

// A convenience function for converting a pointer to
// a uint32 to a pointer to a an int.
//
// If the input is nil, nil will be returned.
func GetUint32PtrToIntPtr(u32 *uint32) *int {
	if u32 == nil {
		return nil
	}

	i := int(*u32)

	return &i
}

func GetUint32Ptr(i uint32) *uint32 {
	return (&i)
}
