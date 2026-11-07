// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
)

func TestParseOptionalBoolQueryParam(t *testing.T) {
	tests := []struct {
		name      string
		rawQuery  string
		wantNil   bool
		want      bool
		expectErr bool
	}{
		{name: "absent param returns nil", rawQuery: "", wantNil: true},
		{name: "true", rawQuery: "flag=true", want: true},
		{name: "false", rawQuery: "flag=false", want: false},
		{name: "1 is true", rawQuery: "flag=1", want: true},
		{name: "0 is false", rawQuery: "flag=0", want: false},
		{name: "invalid value errors", rawQuery: "flag=notabool", expectErr: true},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/?"+tc.rawQuery, nil)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			got, err := ParseOptionalBoolQueryParam(c, "flag")
			assert.Equal(t, tc.expectErr, err != nil)
			if tc.expectErr {
				return
			}
			if tc.wantNil {
				assert.Nil(t, got)
				return
			}
			assert.NotNil(t, got)
			assert.Equal(t, tc.want, *got)
		})
	}
}
