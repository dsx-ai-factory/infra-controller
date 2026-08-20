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
	userData, err := EnablePhoneHomeInUserData(new(`autoinstall:
  version: 1
`), "http://169.254.169.254/phone_home")
	require.NoError(t, err)

	documentRoot := unmarshalDocumentRoot(t, *userData)
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
			userData, err := EnablePhoneHomeInUserData(&tt.userData, phoneHomeURL)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)

			documentRoot := unmarshalDocumentRoot(t, *userData)
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
			original := `phone_home:
  url: http://169.254.169.254/phone-home
autoinstall:
  version: 1
  user-data:
    phone_home:
      url: http://169.254.169.254/phone-home
`

			userData, err := disablePhoneHome(&original, tt.url)
			require.NoError(t, err)

			documentRoot := unmarshalDocumentRoot(t, *userData)
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
				assert.Equal(t, original, *userData, "user-data with no block of ours must come back as authored")
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
		userData, err := EnablePhoneHomeInUserData(new(archive), phoneHomeURL)
		require.NoError(t, err)

		rendered := *userData
		archiveRoot := unmarshalArchiveRoot(t, rendered)

		// The original two entries survive and a third is appended.
		require.Len(t, archiveRoot.Content, 3)

		assert.True(t, strings.HasPrefix(rendered, "#cloud-config-archive\n"),
			"archive header must be preserved: %s", rendered)
		assert.Contains(t, rendered, "echo hi", "existing entries must be preserved")

		// The appended entry must be a text/cloud-config part whose content is a
		// valid #cloud-config carrying the phone-home block.
		appended := archiveRoot.Content[2]
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
		userData, err := EnablePhoneHomeInUserData(&withPhoneHome, phoneHomeURL)
		require.NoError(t, err)

		rendered := *userData
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
		_, err := EnablePhoneHomeInUserData(&headerless, phoneHomeURL)
		assert.ErrorIs(t, err, ErrUnsupportedUserData)
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
			userData, err := disablePhoneHome(new(archive()), tt.url)
			require.NoError(t, err)

			rendered := *userData
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
	userData, err := disablePhoneHome(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://169.254.169.254/phone-home
`), nil)
	require.NoError(t, err)

	rendered := *userData
	assert.Empty(t, unmarshalArchiveRoot(t, rendered).Content, "the only entry must be removed")
	assert.Equal(t, "#cloud-config-archive\n[]\n", rendered,
		"header must be preserved on the emptied archive")
}

func TestRemovePhoneHomeFromArchivePreservesUnsupportedEntry(t *testing.T) {
	// A text/cloud-config entry whose content is really a script (its header
	// conflicts) cannot carry phone-home. Disabling must skip it without error
	// and leave it unchanged, while still removing the genuine phone-home entry.
	const script = "#!/bin/bash\nexport FOO: bar\n"

	userData, err := disablePhoneHome(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    #!/bin/bash
    export FOO: bar
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://169.254.169.254/phone-home
`), nil)
	require.NoError(t, err)

	archiveRoot := unmarshalArchiveRoot(t, *userData)
	require.Len(t, archiveRoot.Content, 1, "only the phone-home entry must be removed")
	assert.Equal(t, script, mappingNodeValue(archiveRoot.Content[0], archiveEntryContent).Value,
		"the script entry must be left unchanged")
	assert.NotContains(t, *userData, "phone_home")
}

func TestRemovePhoneHomeFromArchivePreservesHeaderOnCommentedEntry(t *testing.T) {
	// The header-carrying first entry is removed (it was phone-home only) and the
	// new first entry already has its own comment; the archive header must still
	// be restored so the document stays a valid #cloud-config-archive.
	userData, err := disablePhoneHome(new(`#cloud-config-archive
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
`), nil)
	require.NoError(t, err)

	rendered := *userData
	require.Len(t, unmarshalArchiveRoot(t, rendered).Content, 1)
	assert.True(t, strings.HasPrefix(rendered, "#cloud-config-archive\n"),
		"header must survive removal of the first entry: %s", rendered)
	assert.Contains(t, rendered, "user note", "the entry's own comment must be kept")
	assert.NotContains(t, rendered, "phone_home")
}

func TestRemovePhoneHomeFromArchiveRemovesNestedAutoinstall(t *testing.T) {
	// phone-home nested under autoinstall.user-data inside an archive entry must
	// be detected and re-rendered out, even though the entry's top-level keys
	// are unchanged (so a len(root.Content) comparison would miss it).
	userData, err := disablePhoneHome(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    autoinstall:
      version: 1
      user-data:
        phone_home:
          url: http://169.254.169.254/phone-home
`), nil)
	require.NoError(t, err)

	rendered := *userData
	require.Len(t, unmarshalArchiveRoot(t, rendered).Content, 1, "the entry must be kept - autoinstall remains")
	assert.NotContains(t, rendered, "phone_home", "nested phone-home must be removed")
	assert.Contains(t, rendered, "autoinstall", "the rest of the entry must be preserved")
}

func TestPhoneHomeSupportsUserData(t *testing.T) {
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
		// A jinja template declares its format on the line below the marker, so
		// the two-line header decides - not the marker itself.
		{"jinja #cloud-config", "## template: jinja\n#cloud-config\npackages:\n- curl\n", true},
		{
			"jinja #cloud-config-archive",
			"## template: jinja\n#cloud-config-archive\n- type: text/cloud-config\n  content: x\n",
			true,
		},
		{"jinja #!/bin/bash script parsed as a scalar", "## template: jinja\n#!/bin/bash\necho hello\nls -la\n", false},
		{"jinja #!/bin/bash script parsed as a mapping", "## template: jinja\n#!/bin/bash\nexport FOO: bar\n", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := EnablePhoneHomeInUserData(&tt.userData, "http://169.254.169.254/phone-home")
			if tt.want {
				assert.NoError(t, err)
				return
			}

			assert.ErrorIs(t, err, ErrUnsupportedUserData)

			_, err = DisablePhoneHomeInUserData(&tt.userData, "http://169.254.169.254/phone-home")
			assert.ErrorIs(t, err, ErrUnsupportedUserData, "disabling must report it too, not mangle it")
		})
	}
}

func TestPhoneHomePreservesUserDataHeader(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	// yaml keeps the header on the document's first key or archive entry - which
	// is exactly what a removal can take away - and a jinja template loses its
	// templating altogether if the "## template: jinja" line goes missing.
	tests := []struct {
		name       string
		userData   string
		wantHeader string
	}{
		{
			name:       "jinja #cloud-config",
			userData:   "## template: jinja\n#cloud-config\nhostname: \"{{ v1.local_hostname }}\"\nphone_home:\n  url: " + phoneHomeURL + "\n",
			wantHeader: "## template: jinja\n#cloud-config\n",
		},
		{
			name:       "jinja #cloud-config carrying the header on the phone-home key",
			userData:   "## template: jinja\n#cloud-config\nphone_home:\n  url: " + phoneHomeURL + "\nhostname: \"{{ v1.local_hostname }}\"\n",
			wantHeader: "## template: jinja\n#cloud-config\n",
		},
		{
			name:       "#cloud-config carrying the header on the phone-home key",
			userData:   "#cloud-config\nphone_home:\n  url: " + phoneHomeURL + "\npackages:\n- curl\n",
			wantHeader: "#cloud-config\n",
		},
		{
			name: "jinja #cloud-config-archive carrying the header on the phone-home entry",
			userData: `## template: jinja
#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: ` + phoneHomeURL + `
- type: text/cloud-config
  content: |
    #cloud-config
    hostname: "{{ v1.local_hostname }}"
`,
			wantHeader: "## template: jinja\n#cloud-config-archive\n",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			enabled, err := EnablePhoneHomeInUserData(&tt.userData, phoneHomeURL)
			require.NoError(t, err)
			assertUserDataHeader(t, tt.wantHeader, *enabled)
			assert.Contains(t, *enabled, phoneHomeURL)

			disabled, err := DisablePhoneHomeInUserData(&tt.userData, phoneHomeURL)
			require.NoError(t, err)
			assertUserDataHeader(t, tt.wantHeader, *disabled)
			assert.NotContains(t, *disabled, SitePhoneHomeName)
		})
	}
}

func TestPhoneHomeInUserDataWithNothingInIt(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	// Enabling with no user-data yields a document holding just the block;
	// disabling has nothing to remove and nothing to store.
	for _, userData := range []*string{nil, new("")} {
		enabled, err := EnablePhoneHomeInUserData(userData, phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, "#cloud-config\nphone_home:\n  post: all\n  url: "+phoneHomeURL+"\n", *enabled)

		disabled, err := DisablePhoneHomeInUserData(userData, phoneHomeURL)
		require.NoError(t, err)
		assert.Nil(t, disabled)
	}
}

func TestSplitUserDataHeader(t *testing.T) {
	tests := []struct {
		name       string
		userData   string
		wantHeader string
		wantBody   string
	}{
		{"no header", "packages: []\n", "", "packages: []\n"},
		{"#cloud-config", "#cloud-config\npackages: []\n", "#cloud-config\n", "packages: []\n"},
		{
			"jinja takes the line below the marker",
			"## template: jinja\n#cloud-config\npackages: []\n",
			"## template: jinja\n#cloud-config\n",
			"packages: []\n",
		},
		{
			// An author's own comment is not part of the header: it stays in the
			// body, where yaml keeps it attached to its key.
			"a comment below the header stays in the body",
			"#cloud-config\n# about packages\npackages: []\n",
			"#cloud-config\n",
			"# about packages\npackages: []\n",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			header, body := splitUserDataHeader(tt.userData)
			assert.Equal(t, tt.wantHeader, header)
			assert.Equal(t, tt.wantBody, body)
		})
	}
}

// assertUserDataHeader checks that user-data opens with wantHeader and carries it
// exactly once, so neither a lost nor a duplicated header slips by.
func assertUserDataHeader(t *testing.T, wantHeader, userData string) {
	t.Helper()

	assert.True(t, strings.HasPrefix(userData, wantHeader),
		"user-data must keep its %q header: %s", wantHeader, userData)
	assert.Equal(t, 1, strings.Count(userData, wantHeader),
		"the header must not be duplicated: %s", userData)
}

func unmarshalArchiveRoot(t *testing.T, userData string) *yaml.Node {
	t.Helper()

	archiveRoot := unmarshalUserDataRoot(t, userData)
	require.Equal(t, yaml.SequenceNode, archiveRoot.Kind)

	return archiveRoot
}

func unmarshalDocumentRoot(t *testing.T, userData string) *yaml.Node {
	t.Helper()

	documentRoot := unmarshalUserDataRoot(t, userData)
	require.Equal(t, yaml.MappingNode, documentRoot.Kind)

	return documentRoot
}

func unmarshalUserDataRoot(t *testing.T, userData string) *yaml.Node {
	t.Helper()

	document := &yaml.Node{}
	require.NoError(t, yaml.Unmarshal([]byte(userData), document))
	require.Len(t, document.Content, 1)

	return document.Content[0]
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

	t.Run("nested document keeps the indent it was written with", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(nested), phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, nested+"phone_home:\n  post: all\n  url: "+phoneHomeURL+"\n", *userData,
			"nothing but the block may change")
	})

	t.Run("document near the cap still fits once phone-home is inserted", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(nearCap.String()), phoneHomeURL)
		require.NoError(t, err)
		assert.LessOrEqual(t, len(*userData), MaxUserDataBytes)
		assert.NoError(t, ValidateEffectiveUserData(userData))
	})
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
