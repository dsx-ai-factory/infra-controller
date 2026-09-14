// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"fmt"
	"strings"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"gopkg.in/yaml.v3"
)

func TestInsertedPhoneHomeMatchesCloudInitSchema(t *testing.T) {
	documentRoot := unmarshalDocumentRoot(t, `autoinstall:
  version: 1
`)
	require.NoError(t, InsertPhoneHomeIntoUserData(documentRoot, "http://169.254.169.254/phone_home"))

	autoinstallNode := mappingNodeValue(documentRoot, "autoinstall")
	require.NotNil(t, autoinstallNode)
	targetUserDataNode := mappingNodeValue(autoinstallNode, "user-data")
	require.NotNil(t, targetUserDataNode)

	var targetUserData any
	require.NoError(t, targetUserDataNode.Decode(&targetUserData))

	compiler := jsonschema.NewCompiler()
	compiler.AssertFormat()
	schema, err := compiler.Compile("testdata/cloud-init-phone-home.schema.json")
	require.NoError(t, err)
	require.NoError(t, schema.Validate(targetUserData))
}

func TestInsertPhoneHomeIntoUserData(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	tests := []struct {
		name       string
		userData   string
		wantNested bool
		wantErr    bool
	}{
		{
			name:     "ordinary cloud-init inserts at document root",
			userData: "packages:\n  - curl\n",
		},
		{
			name: "autoinstall inserts into existing target user-data",
			userData: `autoinstall:
  version: 1
  user-data:
    timezone: Etc/UTC
phone_home:
  url: http://stale
`,
			wantNested: true,
		},
		{
			name: "autoinstall creates target user-data",
			userData: `autoinstall:
  version: 1
`,
			wantNested: true,
		},
		{
			name: "rejects non-mapping autoinstall user-data",
			userData: `autoinstall:
  version: 1
  user-data: invalid
`,
			wantNested: true,
			wantErr:    true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			documentRoot := unmarshalDocumentRoot(t, tt.userData)

			err := InsertPhoneHomeIntoUserData(documentRoot, phoneHomeURL)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)

			rootPhoneHome := mappingNodeValue(documentRoot, SitePhoneHomeName)
			autoinstallNode := mappingNodeValue(documentRoot, "autoinstall")
			var targetPhoneHome *yaml.Node
			if autoinstallNode != nil {
				targetUserDataNode := mappingNodeValue(autoinstallNode, "user-data")
				require.NotNil(t, targetUserDataNode)
				targetPhoneHome = mappingNodeValue(targetUserDataNode, SitePhoneHomeName)
			}

			if tt.wantNested {
				assert.Nil(t, rootPhoneHome)
			} else {
				targetPhoneHome = rootPhoneHome
			}
			require.NotNil(t, targetPhoneHome)
			assert.Equal(t, phoneHomeURL, mappingNodeValue(targetPhoneHome, SitePhoneHomeUrl).Value)
			assert.Equal(t, SitePhoneHomePostAll, mappingNodeValue(targetPhoneHome, SitePhoneHomePost).Value)
		})
	}
}

func TestRemovePhoneHomeFromUserData(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	tests := []struct {
		name        string
		url         *string
		wantRemoved bool
	}{
		{
			name:        "removes all phone-home blocks from both locations",
			wantRemoved: true,
		},
		{
			name:        "removes matching phone-home blocks from both locations",
			url:         stringPointer(phoneHomeURL),
			wantRemoved: true,
		},
		{
			name: "preserves non-matching phone-home blocks in both locations",
			url:  stringPointer("http://different"),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			documentRoot := unmarshalDocumentRoot(t, `phone_home:
  url: http://169.254.169.254/phone-home
autoinstall:
  version: 1
  user-data:
    phone_home:
      url: http://169.254.169.254/phone-home
`)

			require.NoError(t, RemovePhoneHomeFromUserData(documentRoot, tt.url))

			rootPhoneHome := mappingNodeValue(documentRoot, SitePhoneHomeName)
			autoinstallNode := mappingNodeValue(documentRoot, "autoinstall")
			targetUserDataNode := mappingNodeValue(autoinstallNode, "user-data")
			targetPhoneHome := mappingNodeValue(targetUserDataNode, SitePhoneHomeName)
			if tt.wantRemoved {
				assert.Nil(t, rootPhoneHome)
				assert.Nil(t, targetPhoneHome)
			} else {
				assert.NotNil(t, rootPhoneHome)
				assert.NotNil(t, targetPhoneHome)
			}
		})
	}
}

func TestMarshalUserData(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/latest/meta-data/phone_home"

	// A nested document at the 2-space indent cloud-config is conventionally
	// written with. Every retained size case submits a flat single-key string,
	// which no encoder re-indents, so nothing else catches inflation here.
	const nested = `#cloud-config
package_update: true
users:
  - name: svc
    groups:
      - wheel
      - docker
write_files:
  - path: /etc/app.conf
    content: |
      listen = 0.0.0.0:8080
      workers = 8
`

	var nearCap strings.Builder
	nearCap.WriteString("#cloud-config\nwrite_files:\n")
	for i := 0; nearCap.Len() < MaxUserDataBytes-1024; i++ {
		fmt.Fprintf(&nearCap, `  - path: /etc/app.d/%d.conf
    owner: root:root
    content: |
      workers = 8
`, i)
	}

	tests := []struct {
		name     string
		userData string
		insert   bool
		check    func(t *testing.T, out string)
	}{
		{
			name:     "nested document round-trips byte for byte",
			userData: nested,
			check: func(t *testing.T, out string) {
				assert.Equal(t, nested, out)
			},
		},
		{
			name:     "document near the cap still fits once phone-home is inserted",
			userData: nearCap.String(),
			insert:   true,
			check: func(t *testing.T, out string) {
				assert.LessOrEqual(t, len(out), MaxUserDataBytes)
				assert.NoError(t, ValidateEffectiveUserData(&out))
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			document := &yaml.Node{}
			require.NoError(t, yaml.Unmarshal([]byte(tt.userData), document))

			if tt.insert {
				require.NoError(t, InsertPhoneHomeIntoUserData(document.Content[0], phoneHomeURL))
			}

			out, err := MarshalUserData(document)
			require.NoError(t, err)

			tt.check(t, string(out))
		})
	}
}

func unmarshalDocumentRoot(t *testing.T, userData string) *yaml.Node {
	t.Helper()

	document := &yaml.Node{}
	require.NoError(t, yaml.Unmarshal([]byte(userData), document))
	require.Len(t, document.Content, 1)
	require.Equal(t, yaml.MappingNode, document.Content[0].Kind)

	return document.Content[0]
}

func stringPointer(value string) *string {
	return &value
}

func mappingNodeValue(mappingNode *yaml.Node, key string) *yaml.Node {
	if mappingNode == nil || mappingNode.Kind != yaml.MappingNode {
		return nil
	}

	for i := 0; i+1 < len(mappingNode.Content); i += 2 {
		keyNode := mappingNode.Content[i]
		if keyNode.Kind == yaml.ScalarNode && keyNode.Value == key {
			return mappingNode.Content[i+1]
		}
	}

	return nil
}
