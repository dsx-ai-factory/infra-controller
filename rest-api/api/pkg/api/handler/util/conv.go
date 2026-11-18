// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"
)

// A convenience function for converting a pointer to
// a native Go integer to a pointer to a uint32 for
// use with a protobuf message. Accepts a pointer to
// an int and returns a uint32 pointer.
//
// If the input is nil, nil will be returned.
// If a pointer to a value greater than
// uint32 max is submitted, an error will be returned.
func GetIntPtrToUint32Ptr(i *int) (*uint32, error) {
	if i == nil {
		return nil, nil
	}

	if *i > math.MaxUint32 {
		return nil, errors.New("conversion to uint32 pointer would exceed uint32 max")
	}

	i32 := uint32(*i)

	return &i32, nil
}

// A convenience function for converting a string to
// a *uint32 for use  with a protobuf message.
func StringToUint32Ptr(s string) (*uint32, error) {

	// Try to convert to a uint
	vI64, err := strconv.ParseUint(s, 10, 0)
	if err != nil {
		return nil, err
	}

	vI := int(vI64)

	// Now, try to convert to a *uint32
	vU32p, err := GetIntPtrToUint32Ptr(&vI)
	if err != nil {
		return nil, err
	}

	return vU32p, nil
}

// A convenience function for converting a string of the
// form "<start>-<end>"to a pair of *uint32 representing
// the start and end of a port range.
//
// Spaces will be stripped automatically.
func StringToPortRangeUint32PtrPair(s string) (start *uint32, end *uint32, err error) {
	// We'll let the atoi handle bad values, but we
	// can clean out space to be helpful.
	s = strings.ReplaceAll(s, " ", "")

	// Split to get our range parts
	rangeParts := strings.Split(s, "-")

	// Err if there are too many parts
	if len(rangeParts) > 2 {
		return nil, nil, fmt.Errorf("encountered invalid port range `%s` in API request", s)
	}

	if len(rangeParts) > 0 {

		start, err = StringToUint32Ptr(rangeParts[0])
		if err != nil {
			return nil, nil, fmt.Errorf("invalid port range start value `%s` in API request: %w", rangeParts[0], err)
		}

		end, err = StringToUint32Ptr(rangeParts[0])
	}

	if len(rangeParts) > 1 {
		end, err = StringToUint32Ptr(rangeParts[1])
		if err != nil {
			return nil, nil, fmt.Errorf("invalid port range end value `%s` in API request: %w", rangeParts[1], err)
		}
	}

	return start, end, err
}

// A convenience function for converting a *string hold a string
// of the form "<start>-<end>"to a pair of *uint32 representing
// the start and end of a port range.
//
// Spaces will be stripped automatically.  If nil is passed in,
// the resulting *uint32 values will be nil.
func StringPtrToPortRangeUint32PtrPair(s *string) (start *uint32, end *uint32, err error) {

	if s == nil {
		return nil, nil, nil
	}

	return StringToPortRangeUint32PtrPair(*s)
}
