// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"google.golang.org/protobuf/types/known/fieldmaskpb"
)

func validateExpectedComponentCredentialPair(username, password *string, usernameField, passwordField string) error {
	if username != nil && password == nil {
		return validation.Errors{passwordField: fmt.Errorf("must be provided together with %s", usernameField)}
	}
	if username == nil && password != nil {
		return validation.Errors{usernameField: fmt.Errorf("must be provided together with %s", passwordField)}
	}
	return nil
}

type expectedComponentUpdateField struct {
	path    string
	present bool
}

func expectedComponentUpdateMask(fields ...expectedComponentUpdateField) *fieldmaskpb.FieldMask {
	mask := &fieldmaskpb.FieldMask{}
	for _, field := range fields {
		if field.present {
			mask.Paths = append(mask.Paths, field.path)
		}
	}
	return mask
}
