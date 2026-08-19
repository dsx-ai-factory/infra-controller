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
			url:         new(phoneHomeURL),
			wantRemoved: true,
		},
		{
			name: "preserves non-matching phone-home blocks in both locations",
			url:  new("http://different"),
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

			_, err := RemovePhoneHomeFromUserData(documentRoot, tt.url)
			require.NoError(t, err)

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

func TestInsertPhoneHomeIntoArchive(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	const archive = `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    packages:
    - curl
- type: text/x-shellscript
  content: |
    #!/bin/sh
    echo hi
`

	t.Run("appends a phone-home cloud-config entry and preserves the archive", func(t *testing.T) {
		documentRoot := unmarshalArchiveRoot(t, archive)

		require.NoError(t, InsertPhoneHomeIntoUserData(documentRoot, phoneHomeURL))

		// The original two entries survive and a third is appended.
		require.Len(t, documentRoot.Content, 3)

		rendered := marshalDocument(t, documentRoot)
		assert.True(t, strings.HasPrefix(rendered, "#cloud-config-archive\n"),
			"archive header must be preserved: %s", rendered)
		assert.Contains(t, rendered, "echo hi", "existing entries must be preserved")

		// The appended entry must be a text/cloud-config part whose content is a
		// valid #cloud-config carrying the phone-home block.
		appended := documentRoot.Content[2]
		assert.Equal(t, archiveContentType, mappingNodeValue(appended, archiveEntryType).Value)

		content := mappingNodeValue(appended, archiveEntryContent).Value
		phoneHome := phoneHomeFromContent(t, content)
		require.NotNil(t, phoneHome)
		assert.Equal(t, phoneHomeURL, mappingNodeValue(phoneHome, SitePhoneHomeUrl).Value)
		assert.Equal(t, SitePhoneHomePostAll, mappingNodeValue(phoneHome, SitePhoneHomePost).Value)
	})

	t.Run("replaces a standalone phone-home entry instead of duplicating it", func(t *testing.T) {
		withPhoneHome := archive + `- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://existing
`
		documentRoot := unmarshalArchiveRoot(t, withPhoneHome)

		require.NoError(t, InsertPhoneHomeIntoUserData(documentRoot, phoneHomeURL))

		rendered := marshalDocument(t, documentRoot)
		assert.Contains(t, rendered, phoneHomeURL)
		assert.NotContains(t, rendered, "http://existing", "the stale phone-home entry must be replaced")
		assert.Equal(t, 1, strings.Count(rendered, "phone_home:"), "exactly one phone-home entry must remain")
	})

	t.Run("does not treat a header-less list as a cloud-config-archive", func(t *testing.T) {
		// A YAML list with no #cloud-config-archive header is not valid cloud-init
		// user-data, so phone-home must not be enabled on it.
		headerless := `- type: text/cloud-config
  content: |
    #cloud-config
    packages:
    - curl
`
		documentRoot := unmarshalArchiveRoot(t, headerless)

		assert.False(t, PhoneHomeSupportsUserDataRoot(documentRoot))
		assert.Error(t, InsertPhoneHomeIntoUserData(documentRoot, phoneHomeURL))
	})
}

func TestRemovePhoneHomeFromArchive(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	archive := func() string {
		return `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: ` + phoneHomeURL + `
- type: text/x-shellscript
  content: |
    #!/bin/sh
    echo hi
`
	}

	tests := []struct {
		name        string
		url         *string
		wantRemoved bool
	}{
		{name: "removes any phone-home entry when url is nil", wantRemoved: true},
		{name: "removes the matching phone-home entry", url: new(phoneHomeURL), wantRemoved: true},
		{name: "keeps a non-matching phone-home entry", url: new("http://different")},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			documentRoot := unmarshalArchiveRoot(t, archive())

			_, err := RemovePhoneHomeFromUserData(documentRoot, tt.url)
			require.NoError(t, err)

			rendered := marshalDocument(t, documentRoot)
			assert.True(t, strings.HasPrefix(rendered, "#cloud-config-archive\n"),
				"archive header must survive removal: %s", rendered)
			assert.Contains(t, rendered, "echo hi", "unrelated entries must be kept")

			if tt.wantRemoved {
				assert.NotContains(t, rendered, "phone_home")
			} else {
				assert.Contains(t, rendered, "phone_home")
			}
		})
	}
}

func TestRemovePhoneHomeFromArchivePreservesHeaderWhenEmptied(t *testing.T) {
	// An archive whose only entry is phone-home becomes empty on removal, but
	// must keep its #cloud-config-archive header so it stays valid cloud-init.
	documentRoot := unmarshalArchiveRoot(t, `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://169.254.169.254/phone-home
`)

	_, err := RemovePhoneHomeFromUserData(documentRoot, nil)
	require.NoError(t, err)

	assert.Empty(t, documentRoot.Content, "the only entry must be removed")

	rendered := marshalDocument(t, documentRoot)
	assert.True(t, strings.HasPrefix(rendered, "#cloud-config-archive\n"),
		"header must be preserved on the emptied archive: %s", rendered)
	assert.NotContains(t, rendered, "phone_home")
}

func TestRemovePhoneHomeFromArchivePreservesUnsupportedEntry(t *testing.T) {
	// A text/cloud-config entry whose content is really a script (its header
	// conflicts) cannot carry phone-home. Disabling must skip it without error
	// and leave it unchanged, while still removing the genuine phone-home entry.
	const script = "#!/bin/bash\nexport FOO: bar\n"

	documentRoot := unmarshalArchiveRoot(t, `#cloud-config-archive
- type: text/cloud-config
  content: |
    #!/bin/bash
    export FOO: bar
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://169.254.169.254/phone-home
`)

	_, err := RemovePhoneHomeFromUserData(documentRoot, nil)
	require.NoError(t, err)

	require.Len(t, documentRoot.Content, 1, "only the phone-home entry must be removed")
	assert.Equal(t, script, mappingNodeValue(documentRoot.Content[0], archiveEntryContent).Value,
		"the script entry must be left unchanged")
	assert.NotContains(t, marshalDocument(t, documentRoot), "phone_home")
}

func TestRemovePhoneHomeFromArchivePreservesHeaderOnCommentedEntry(t *testing.T) {
	// The header-carrying first entry is removed (it was phone-home only) and the
	// new first entry already has its own comment; the archive header must still
	// be restored so the document stays a valid #cloud-config-archive.
	documentRoot := unmarshalArchiveRoot(t, `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://169.254.169.254/phone-home
# user note
- type: text/cloud-config
  content: |
    #cloud-config
    packages:
    - curl
`)

	_, err := RemovePhoneHomeFromUserData(documentRoot, nil)
	require.NoError(t, err)

	require.Len(t, documentRoot.Content, 1)
	rendered := marshalDocument(t, documentRoot)
	assert.True(t, strings.HasPrefix(rendered, "#cloud-config-archive\n"),
		"header must survive removal of the first entry: %s", rendered)
	assert.Contains(t, rendered, "user note", "the entry's own comment must be kept")
	assert.NotContains(t, rendered, "phone_home")
}

func TestRemovePhoneHomeFromArchiveRemovesNestedAutoinstall(t *testing.T) {
	// phone-home nested under autoinstall.user-data inside an archive entry must
	// be detected and re-rendered out, even though the entry's top-level keys
	// are unchanged (so a len(root.Content) comparison would miss it).
	documentRoot := unmarshalArchiveRoot(t, `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    autoinstall:
      version: 1
      user-data:
        phone_home:
          url: http://169.254.169.254/phone-home
`)

	_, err := RemovePhoneHomeFromUserData(documentRoot, nil)
	require.NoError(t, err)

	require.Len(t, documentRoot.Content, 1, "the entry must be kept - autoinstall remains")
	rendered := marshalDocument(t, documentRoot)
	assert.NotContains(t, rendered, "phone_home", "nested phone-home must be removed")
	assert.Contains(t, rendered, "autoinstall", "the rest of the entry must be preserved")
}

func TestPhoneHomeSupportsUserDataRoot(t *testing.T) {
	tests := []struct {
		name     string
		userData string
		want     bool
	}{
		{"#cloud-config mapping", "#cloud-config\npackages:\n- curl\n", true},
		{"header-less mapping is auto-corrected", "packages:\n- curl\n", true},
		{"empty mapping", "{}\n", true},
		{"#cloud-config-archive", "#cloud-config-archive\n- type: text/cloud-config\n  content: x\n", true},
		{"empty #cloud-config-archive", "#cloud-config-archive\n[]\n", true},
		{"header-less list is not an archive", "- type: text/cloud-config\n  content: x\n", false},
		{"#!/bin/bash script parsed as a scalar", "#!/bin/bash\necho hello\nls -la\n", false},
		{"#!/bin/bash script parsed as a mapping", "#!/bin/bash\nexport FOO: bar\n", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			document := &yaml.Node{}
			require.NoError(t, yaml.Unmarshal([]byte(tt.userData), document))

			var root *yaml.Node
			if len(document.Content) > 0 {
				root = document.Content[0]
			}

			assert.Equal(t, tt.want, PhoneHomeSupportsUserDataRoot(root))
		})
	}
}

func unmarshalArchiveRoot(t *testing.T, userData string) *yaml.Node {
	t.Helper()

	document := &yaml.Node{}
	require.NoError(t, yaml.Unmarshal([]byte(userData), document))
	require.Len(t, document.Content, 1)
	require.Equal(t, yaml.SequenceNode, document.Content[0].Kind)

	return document.Content[0]
}

func marshalDocument(t *testing.T, documentRoot *yaml.Node) string {
	t.Helper()

	out, err := yaml.Marshal(&yaml.Node{Kind: yaml.DocumentNode, Content: []*yaml.Node{documentRoot}})
	require.NoError(t, err)

	return string(out)
}

func phoneHomeFromContent(t *testing.T, content string) *yaml.Node {
	t.Helper()

	inner := &yaml.Node{}
	require.NoError(t, yaml.Unmarshal([]byte(content), inner))
	require.Len(t, inner.Content, 1)

	return mappingNodeValue(inner.Content[0], SitePhoneHomeName)
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
