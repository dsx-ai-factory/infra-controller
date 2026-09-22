// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"errors"
	"fmt"
	"net/http"
	"strings"
	"testing"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestValidateLabels(t *testing.T) {
	tooMany := make(map[string]string)
	for i := range LabelCountMax + 1 {
		tooMany[fmt.Sprintf("k%d", i)] = "v"
	}
	tests := []struct {
		name    string
		labels  map[string]string
		wantErr error
	}{
		{name: "nil labels"},
		{name: "empty labels", labels: map[string]string{}},
		{name: "too many labels", labels: tooMany, wantErr: ErrValidationLabelCount},
		{name: "empty key", labels: map[string]string{"": "v"}, wantErr: ErrValidationLabelKeyEmpty},
		{name: "whitespace key", labels: map[string]string{"   ": "v"}, wantErr: errors.New("label key consists only of whitespace")},
		{name: "value too long", labels: map[string]string{"k": strings.Repeat("v", LabelValueMaxLength+1)}, wantErr: ErrValidationLabelValueLength},
		{name: "NUL key", labels: map[string]string{"k\x00": "v"}, wantErr: ErrValidationLabelNUL},
		{name: "NUL value", labels: map[string]string{"k": "v\x00"}, wantErr: ErrValidationLabelNUL},
		{name: "valid labels", labels: map[string]string{"ok": "ok"}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := ValidateLabels(tt.labels)
			if tt.wantErr == nil {
				require.NoError(t, err)
				return
			}
			require.Error(t, err)
			var validationErrors validation.Errors
			require.ErrorAs(t, err, &validationErrors)
			assert.Equal(t, tt.wantErr.Error(), validationErrors["labels"].Error())
		})
	}
}

func TestValidateNameCharacters(t *testing.T) {
	val := 0
	// test error when string not passed
	assert.NotNil(t, ValidateNameCharacters(val))
	assert.NotNil(t, ValidateNameCharacters(&val))
	assert.NotNil(t, ValidateNameCharacters(nil))
	tests := []struct {
		desc      string
		names     []string
		expectErr bool
	}{
		{
			desc:      "error with leading whitespaces",
			names:     []string{" hello", "\thello", "\nhello", "     "},
			expectErr: true,
		},
		{
			desc:      "errors with trailing whitespaces",
			names:     []string{"hello ", "hello\t", "hello\n"},
			expectErr: true,
		},
		{
			desc:      "success cases",
			names:     []string{"hel lo", "hel \t lo", "hel&&lo"},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, s := range tc.names {
				err := ValidateNameCharacters(s)
				assert.Equal(t, tc.expectErr, err != nil)
				err = ValidateNameCharacters(&s)
				assert.Equal(t, tc.expectErr, err != nil)
			}
		})
	}
}

func TestValidateSitePowerManagement(t *testing.T) {
	value := "balanced"
	empty := ""
	whitespace := "   "

	tests := []struct {
		name       string
		config     *cdbm.SiteConfig
		value      *string
		wantReject bool
	}{
		{name: "missing config rejects value", value: &value, wantReject: true},
		{name: "disabled rejects value", config: &cdbm.SiteConfig{}, value: &value, wantReject: true},
		{name: "enabled accepts value", config: &cdbm.SiteConfig{DPSPowerManagement: true}, value: &value},
		{name: "disabled accepts omission", config: &cdbm.SiteConfig{}},
		{name: "disabled accepts clear", config: &cdbm.SiteConfig{}, value: &empty},
		{name: "disabled rejects whitespace value", config: &cdbm.SiteConfig{}, value: &whitespace, wantReject: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			apiErr := ValidateSitePowerManagement(tt.config, tt.value)
			if !tt.wantReject {
				assert.Nil(t, apiErr)
				return
			}
			require.NotNil(t, apiErr)
			assert.Equal(t, http.StatusPreconditionFailed, apiErr.Code)
		})
	}
}
