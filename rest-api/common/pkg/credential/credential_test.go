// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credential

import (
	"os"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestCredentialEqual(t *testing.T) {
	c1 := New("admin", "pass")
	c2 := New("admin", "secret")
	c3 := New("root", "secret")
	c4 := New("admin", "other")
	c5 := New("root", "other")
	c6 := New("root", "other")

	tests := map[string]struct {
		a      *Credential
		b      *Credential
		expect bool
	}{
		"both nil":           {a: nil, b: nil, expect: true},
		"first nil":          {a: nil, b: &c1, expect: false},
		"second nil":         {a: &c1, b: nil, expect: false},
		"identical":          {a: &c2, b: &c2, expect: true},
		"equal values":       {a: &c5, b: &c6, expect: true},
		"different user":     {a: &c2, b: &c3, expect: false},
		"different password": {a: &c2, b: &c4, expect: false},
		"both differ":        {a: &c2, b: &c5, expect: false},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.expect, tc.a.Equal(tc.b))
		})
	}
}

func TestCredential(t *testing.T) {
	cred := New("testuser", "testpassword")

	assert.True(t, cred.IsValid())

	patched := cred.Patch(nil)
	assert.False(t, patched)

	nc := New("newuser", "newpassword")
	patched = cred.Patch(&nc)
	assert.True(t, patched)
	assert.Equal(t, "newuser", cred.User)
	assert.Equal(t, "newpassword", cred.Password.Value)

	newUser := "updateduser"
	newPassword := "updatedpassword"
	cred.Update(&newUser, &newPassword)
	assert.Equal(t, "updateduser", cred.User)
	assert.Equal(t, "updatedpassword", cred.Password.Value)

	user, password := cred.Retrieve()
	assert.NotNil(t, user)
	assert.NotNil(t, password)
	assert.Equal(t, "updateduser", *user)
	assert.Equal(t, "updatedpassword", *password)
}

func TestNewCredentialFromEnv(t *testing.T) {
	os.Setenv("TEST_USER", "testuser")
	os.Setenv("TEST_PASSWORD", "testpassword")
	defer os.Unsetenv("TEST_USER")
	defer os.Unsetenv("TEST_PASSWORD")

	cred := NewFromEnv("TEST_USER", "TEST_PASSWORD")
	assert.Equal(t, "testuser", cred.User)
	assert.Equal(t, "testpassword", cred.Password.Value)
}

func TestCredentialIsValid(t *testing.T) {
	valid := New("admin", "pass")
	assert.True(t, valid.IsValid())

	empty := New("", "pass")
	assert.False(t, empty.IsValid())

	whitespace := New("   ", "pass")
	assert.False(t, whitespace.IsValid())
}

func TestCredentialRetrieveInvalid(t *testing.T) {
	cred := New("", "pass")
	user, password := cred.Retrieve()
	assert.Nil(t, user)
	assert.Nil(t, password)
}
