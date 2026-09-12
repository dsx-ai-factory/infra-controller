// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"encoding/base64"
	"fmt"
	"strings"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"gopkg.in/yaml.v3"
)

func TestInsertedPhoneHomeMatchesCloudInitSchema(t *testing.T) {
	userData, err := EnablePhoneHomeInUserData(new(`#cloud-config
autoinstall:
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

func TestEnablePhoneHomeInUserData(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	tests := []struct {
		name       string
		userData   string
		wantNested bool
		wantErr    bool
	}{
		{
			name:     "ordinary cloud-init inserts at document root",
			userData: "#cloud-config\npackages:\n  - curl\n",
		},
		{
			name: "autoinstall inserts into existing target user-data",
			userData: `#cloud-config
autoinstall:
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
			userData: `#cloud-config
autoinstall:
  version: 1
`,
			wantNested: true,
		},
		{
			name: "rejects non-mapping autoinstall user-data",
			userData: `#cloud-config
autoinstall:
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

func TestDisablePhoneHomeInUserData(t *testing.T) {
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
			original := `#cloud-config
phone_home:
  url: http://169.254.169.254/phone-home
autoinstall:
  version: 1
  user-data:
    phone_home:
      url: http://169.254.169.254/phone-home
`

			userData, err := disablePhoneHome(&original, tt.url, "")
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

	t.Run("strips a stale block from an entry and keeps the rest of it", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  launch-index: 2
  filename: base.yaml
  content: |
    #cloud-config
    packages:
    - curl
    phone_home:
      url: http://old.example/phone-home
    runcmd:
    - [echo, hi]
`), phoneHomeURL)
		require.NoError(t, err)

		archiveRoot := unmarshalArchiveRoot(t, *userData)
		require.Len(t, archiveRoot.Content, 2, "the entry is kept and phone-home appended to the archive")

		entry := archiveRoot.Content[0]
		assert.Equal(t, "2", mappingNodeValue(entry, "launch-index").Value, "the entry's own keys must be kept")
		assert.Equal(t, "base.yaml", mappingNodeValue(entry, "filename").Value)

		content := mappingNodeValue(entry, archiveEntryContent).Value
		assert.NotContains(t, content, "old.example", "the stale block must go")
		assert.Contains(t, content, "curl", "what was written beside it must not")
		assert.Contains(t, content, "runcmd")
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
			userData, err := disablePhoneHome(new(archive()), tt.url, "")
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
`), nil, "")
	require.NoError(t, err)

	rendered := *userData
	assert.Empty(t, unmarshalArchiveRoot(t, rendered).Content, "the only entry must be removed")
	assert.Equal(t, "#cloud-config-archive\n[]\n", rendered,
		"header must be preserved on the emptied archive")
}

func TestRemovePhoneHomeFromArchivePreservesEntriesItCannotEdit(t *testing.T) {
	// An entry phone-home cannot live in - content that is really a script under a
	// cloud-config type, or a template yaml would write back as a mapping - must
	// be skipped without error and left unchanged, while the genuine phone-home
	// entry is still removed.
	const script = "#!/bin/bash\nexport FOO: bar\nphone_home:\n  url: http://169.254.169.254/phone-home\n"
	const untypedScript = "#cloud-boothook\nexport BAR: baz\nphone_home:\n  url: http://169.254.169.254/phone-home\n"
	const template = "## template: jinja\n#cloud-config\nhostname: {{ v1.local_hostname }}\nphone_home:\n  url: http://169.254.169.254/phone-home\n"

	userData, err := disablePhoneHome(new(`#cloud-config-archive
- content: |
    #!/bin/bash
    export FOO: bar
    phone_home:
      url: http://169.254.169.254/phone-home
- content: |
    ## template: jinja
    #cloud-config
    hostname: {{ v1.local_hostname }}
    phone_home:
      url: http://169.254.169.254/phone-home
- content: |
    #cloud-boothook
    export BAR: baz
    phone_home:
      url: http://169.254.169.254/phone-home
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://removed.example/phone-home
`), nil, "")
	require.NoError(t, err)

	archiveRoot := unmarshalArchiveRoot(t, *userData)
	require.Len(t, archiveRoot.Content, 3, "only the entry it can edit must be removed")
	assert.Equal(t, script, mappingNodeValue(archiveRoot.Content[0], archiveEntryContent).Value,
		"the script entry must be left unchanged")
	assert.Equal(t, template, mappingNodeValue(archiveRoot.Content[1], archiveEntryContent).Value,
		"the template must be left as authored, block and all")
	assert.Equal(t, untypedScript, mappingNodeValue(archiveRoot.Content[2], archiveEntryContent).Value,
		"an entry declaring no type but another format is not cloud-config to rewrite")
	assert.NotContains(t, *userData, "removed.example")
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
`), nil, "")
	require.NoError(t, err)

	rendered := *userData
	require.Len(t, unmarshalArchiveRoot(t, rendered).Content, 1)
	assert.True(t, strings.HasPrefix(rendered, "#cloud-config-archive\n"),
		"header must survive removal of the first entry: %s", rendered)
	assert.Contains(t, rendered, "user note", "the entry's own comment must be kept")
	assert.NotContains(t, rendered, "phone_home")
}

func TestRemovePhoneHomeFromArchivePreservesStructuredTypeEntry(t *testing.T) {
	// A structured "type" is malformed, not an omitted one: cloud-init cannot
	// dispatch the entry, so it must be handed back exactly as authored.
	const archive = `#cloud-config-archive
- type: {invalid: value}
  content: |
    #cloud-config
    phone_home:
      url: http://169.254.169.254/phone-home
`

	userData, err := disablePhoneHome(new(archive), nil, "")
	require.NoError(t, err)
	assert.Equal(t, archive, *userData)
}

func TestPhoneHomeInAnArchiveEntryFormat(t *testing.T) {
	// cloud-init gives an entry that declares no format the text/cloud-config
	// type, which is the form its own documentation writes, so a block in one is
	// live and the entry is where phone-home belongs.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	t.Run("disabling strips the block", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    package_update: true
    phone_home:
      url: `+phoneHomeURL+`
      post: all
`), phoneHomeURL)
		require.NoError(t, err)
		assert.NotContains(t, *userData, SitePhoneHomeName)
		assert.Contains(t, *userData, "package_update", "the rest of the entry must be kept")
	})

	t.Run("enabling reaches the autoinstall in it", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    autoinstall:
      version: 1
`), phoneHomeURL)
		require.NoError(t, err)

		archiveRoot := unmarshalArchiveRoot(t, *userData)
		require.Len(t, archiveRoot.Content, 1, "phone-home must not be added as an entry of its own")
		assert.Equal(t, `autoinstall:
  version: 1
  user-data:
    phone_home:
      post: all
      url: `+phoneHomeURL+`
`, mappingNodeValue(archiveRoot.Content[0], archiveEntryContent).Value,
			"the block belongs under the autoinstall in the entry, which stays as authored")
	})

	t.Run("a first line declaring no format leaves the entry cloud-config", func(t *testing.T) {
		// cloud-init reads no format out of a comment, or out of a marker it does
		// not know, so the entry keeps the type it gives one declaring none.
		for _, firstLine := range []string{"# my config", "#", "#note"} {
			userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- content: |
    `+firstLine+`
    package_update: true
    phone_home:
      url: `+phoneHomeURL+`
`), phoneHomeURL)
			require.NoError(t, err)
			assert.NotContains(t, *userData, SitePhoneHomeName, "first line %q", firstLine)
			assert.Contains(t, *userData, firstLine, "the line is the author's, so it stays")
		}
	})

	t.Run("a declared type wins over a first line contradicting it", func(t *testing.T) {
		// cloud-init dispatches an entry on the type it declares without reading
		// the content, so a first line saying otherwise is a comment to it.
		for _, firstLine := range []string{"#note", "#!/bin/bash", "#cloud-boothook"} {
			userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    `+firstLine+`
    phone_home:
      url: `+phoneHomeURL+`
`), phoneHomeURL)
			require.NoError(t, err)
			assert.NotContains(t, *userData, SitePhoneHomeName, "first line %q", firstLine)
		}
	})

	t.Run("a template is left alone whatever the type declares", func(t *testing.T) {
		// Rendering a template back rewrites the expressions in it, so the entry
		// keeps its own answer - and cloud-init does not render it under this type
		// either, so the block in there is one we report rather than remove.
		const templated = `#cloud-config-archive
- type: text/cloud-config
  content: |
    ## template: jinja
    hostname: {{ v1.local_hostname }}
    phone_home:
      url: ` + phoneHomeURL + `
`

		userData, err := DisablePhoneHomeInUserData(new(templated), phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, templated, *userData)
	})

	t.Run("a type in another case is still cloud-config", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: TEXT/CLOUD-CONFIG
  content: |
    #cloud-config
    phone_home:
      url: `+phoneHomeURL+`
`), phoneHomeURL)
		require.NoError(t, err)
		assert.NotContains(t, *userData, SitePhoneHomeName)
	})
}

func TestPhoneHomeInAliasedArchiveEntry(t *testing.T) {
	// An alias entry is the entry it points at, so the two must be kept in step:
	// a lone alias whose anchor was dropped cannot be parsed back at all.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	t.Run("drops the alias along with the entry it points at", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- &phone
  type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: `+phoneHomeURL+`
- *phone
`), phoneHomeURL)
		require.NoError(t, err)

		// The alias entry is read through to the entry it points at, so it is
		// stripped and dropped with it - rather than kept, which would leave
		// `- *phone` behind with nothing to point at.
		assert.NotContains(t, *userData, "*phone", "no alias may outlive the entry it points at")

		// unmarshalArchiveRoot parses the result, so a dangling alias fails here.
		assert.Empty(t, unmarshalArchiveRoot(t, *userData).Content, "both entries must be removed")
	})

	t.Run("strips the entry an alias points at, and keeps the alias", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- &both
  type: text/cloud-config
  content: |
    #cloud-config
    packages:
    - curl
    phone_home:
      url: `+phoneHomeURL+`
- *both
`), phoneHomeURL)
		require.NoError(t, err)

		archiveRoot := unmarshalArchiveRoot(t, *userData)
		require.Len(t, archiveRoot.Content, 2, "both entries must be kept")
		assert.NotContains(t, *userData, SitePhoneHomeName)
		assert.Equal(t, yaml.AliasNode, archiveRoot.Content[1].Kind,
			"the entry is rewritten where it stands, so the alias to it reads the rewrite")
	})

	t.Run("strips the entry whose content an alias points at", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: &c |
    #cloud-config
    packages:
    - curl
    phone_home:
      url: `+phoneHomeURL+`
- content: *c
`), phoneHomeURL)
		require.NoError(t, err)

		assert.Len(t, unmarshalArchiveRoot(t, *userData).Content, 2, "both entries must be kept")
		assert.NotContains(t, *userData, SitePhoneHomeName)
	})
}

func TestRemovePhoneHomeInlinesAliasesToWhatItRemoves(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	// Removing phone-home takes any anchor inside it away, so an alias to it
	// elsewhere is replaced by the value itself. unmarshal* parses the result,
	// so an alias left dangling fails these tests.
	t.Run("only the url is anchored, and a command reuses it", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
phone_home:
  url: &phonehome `+phoneHomeURL+`
runcmd:
- [curl, -sf, *phonehome]
`), phoneHomeURL)
		require.NoError(t, err)

		documentRoot := unmarshalDocumentRoot(t, *userData)
		assert.Nil(t, mappingNodeValue(documentRoot, SitePhoneHomeName))

		var runcmd [][]string
		require.NoError(t, mappingNodeValue(documentRoot, "runcmd").Decode(&runcmd))
		assert.Equal(t, [][]string{{"curl", "-sf", phoneHomeURL}}, runcmd,
			"the command must keep the url it was authored with")
	})

	t.Run("the whole block is anchored", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
phone_home: &phone
  url: `+phoneHomeURL+`
reuse: *phone
`), phoneHomeURL)
		require.NoError(t, err)

		documentRoot := unmarshalDocumentRoot(t, *userData)
		assert.Nil(t, mappingNodeValue(documentRoot, SitePhoneHomeName))
		assert.Equal(t, phoneHomeURL,
			mappingNodeValue(mappingNodeValue(documentRoot, "reuse"), SitePhoneHomeUrl).Value)
	})

	t.Run("an entry of another type aliases the content", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: &shared |
    #cloud-config
    phone_home:
      url: `+phoneHomeURL+`
- type: text/x-shellscript
  content: *shared
`), phoneHomeURL)
		require.NoError(t, err)

		archiveRoot := unmarshalArchiveRoot(t, *userData)
		require.Len(t, archiveRoot.Content, 1, "only the cloud-config entry must go")
		assert.Contains(t, mappingNodeValue(archiveRoot.Content[0], archiveEntryContent).Value, SitePhoneHomeName,
			"the script entry is not cloud-config, so its text is kept as authored")
	})

	t.Run("an entry of another type aliases content that survives", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: &shared |
    #cloud-config
    packages:
    - curl
    phone_home:
      url: `+phoneHomeURL+`
- type: text/x-shellscript
  content: *shared
`), phoneHomeURL)
		require.NoError(t, err)

		archiveRoot := unmarshalArchiveRoot(t, *userData)
		require.Len(t, archiveRoot.Content, 2, "both entries must be kept")
		assert.NotContains(t, mappingNodeValue(archiveRoot.Content[0], archiveEntryContent).Value, SitePhoneHomeName,
			"the cloud-config entry must be stripped")
		assert.Contains(t, mappingNodeValue(archiveRoot.Content[1], archiveEntryContent).Value, SitePhoneHomeName,
			"the script entry is not cloud-config, so its text is kept as authored")
	})

	t.Run("a comment sits on the alias", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
phone_home:
  url: &phonehome `+phoneHomeURL+`
reported_to: *phonehome # keep me
`), phoneHomeURL)
		require.NoError(t, err)

		// unmarshalDocumentRoot parses the result, so a dangling alias fails here.
		assert.Nil(t, mappingNodeValue(unmarshalDocumentRoot(t, *userData), SitePhoneHomeName))
		assert.Contains(t, *userData, "# keep me", "a comment stays on the line it was written on")
	})
}

func TestPhoneHomeBehindAnAlias(t *testing.T) {
	// cloud-init resolves aliases before it reads the document, so a block behind
	// one is live and has to be read the same way.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	t.Run("the whole block is an alias", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
defaults: &phone
  url: `+phoneHomeURL+`
phone_home: *phone
`), phoneHomeURL)
		require.NoError(t, err)

		assert.Nil(t, mappingNodeValue(unmarshalDocumentRoot(t, *userData), SitePhoneHomeName))
	})

	t.Run("only the url is an alias", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
url: &phonehome `+phoneHomeURL+`
phone_home:
  url: *phonehome
`), phoneHomeURL)
		require.NoError(t, err)

		assert.Nil(t, mappingNodeValue(unmarshalDocumentRoot(t, *userData), SitePhoneHomeName))
	})

	t.Run("autoinstall is an alias", func(t *testing.T) {
		const authored = `#cloud-config
base: &install
  version: 1
  user-data:
    phone_home:
      url: ` + phoneHomeURL + `
autoinstall: *install
`

		userData, err := DisablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)
		assert.NotContains(t, *userData, SitePhoneHomeName,
			"the block under the aliased autoinstall must go")

		userData, err = EnablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, 1, strings.Count(*userData, SitePhoneHomeName),
			"the block must be replaced in the mapping the alias points at")
	})
}

func TestRemovePhoneHomeBoundsWhatItInlines(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	// Inlining an alias walks the value it copies, and an anchor inside
	// phone-home can be pointed at from inside the block as well as from outside
	// it - the block can even point at itself. Both cases below run forever, or
	// run out of memory, if a value is copied more than once.
	t.Run("the block points at itself", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
phone_home: &phone
  url: `+phoneHomeURL+`
  self: *phone
reuse: *phone
`), phoneHomeURL)
		require.NoError(t, err)

		// unmarshalDocumentRoot parses the result, so a dangling alias fails here.
		documentRoot := unmarshalDocumentRoot(t, *userData)
		assert.Nil(t, mappingNodeValue(documentRoot, SitePhoneHomeName))
		assert.Equal(t, phoneHomeURL,
			mappingNodeValue(mappingNodeValue(documentRoot, "reuse"), SitePhoneHomeUrl).Value)
	})

	t.Run("aliases are nested inside aliases", func(t *testing.T) {
		authored := strings.Builder{}
		authored.WriteString("#cloud-config\nphone_home:\n  url: " + phoneHomeURL + "\n  a0: &a0 [x]\n")
		for level := 1; level <= 20; level++ {
			fmt.Fprintf(&authored, "  a%d: &a%d [*a%d, *a%d]\n", level, level, level-1, level-1)
		}
		authored.WriteString("keep: *a20\n")

		userData, err := DisablePhoneHomeInUserData(new(authored.String()), phoneHomeURL)
		require.NoError(t, err)

		// Copying a value per alias instead of keeping its anchor doubles the
		// output per level, which is megabytes of stored user-data by level 20.
		assert.Less(t, len(*userData), 2*authored.Len(), "the output must not grow with the nesting")
	})
}

func TestRenderUserDataReportsWhatYamlCannotReadBack(t *testing.T) {
	// An alias with no anchor to point at cannot be parsed back.
	documentRoot := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map", Content: []*yaml.Node{
		scalarNode("reuse"), {Kind: yaml.AliasNode, Value: "gone"},
	}}

	// Not unsupported user-data: the callers leave that alone on disable, which
	// would report phone-home disabled over user-data still carrying it.
	_, err := renderUserData(SiteCloudConfig+"\n", documentRoot)
	require.Error(t, err)
	assert.NotErrorIs(t, err, ErrUnsupportedUserData)
}

func TestPhoneHomeInAMergedArchiveEntry(t *testing.T) {
	// cloud-init resolves `<<` at load, so an entry merging another entry runs
	// what that entry holds - including one we drop for holding nothing else.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	const authored = `#cloud-config-archive
- &base
  type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: ` + phoneHomeURL + `
- <<: *base
  launch-index: 1
`

	t.Run("disabling rewrites the entry the merge reads", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- &base
  type: text/cloud-config
  content: |
    #cloud-config
    packages:
    - curl
    phone_home:
      url: `+phoneHomeURL+`
- <<: *base
  launch-index: 1
`), phoneHomeURL)
		require.NoError(t, err)

		assert.NotContains(t, *userData, SitePhoneHomeName)
	})

	t.Run("disabling drops the entry the merge reads", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)

		assert.NotContains(t, *userData, SitePhoneHomeName,
			"the dropped entry must not come back through the merge")
	})

	t.Run("strips the block an entry merges its type for", func(t *testing.T) {
		// The type arrives through the merge, so the entry is cloud-config and
		// the block in it is ours to take out.
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- &base
  type: text/cloud-config
- <<: *base
  content: |
    #cloud-config
    packages:
    - curl
    phone_home:
      url: `+phoneHomeURL+`
`), phoneHomeURL)
		require.NoError(t, err)

		assert.NotContains(t, *userData, SitePhoneHomeName)
		assert.Contains(t, *userData, "curl", "the rest of the entry must be kept")
	})

	t.Run("rewrites an entry that merges the content it strips", func(t *testing.T) {
		// The content is not the entry's own to rewrite, so the entry takes one of
		// its own: cloud-init resolves the merge, then reads the entry's key over
		// the merged one.
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/x-shellscript
  content: |
    #!/bin/sh
  base: &base
    type: text/cloud-config
    content: |
      #cloud-config
      packages:
      - curl
      phone_home:
        url: `+phoneHomeURL+`
- <<: *base
  launch-index: 1
`), phoneHomeURL)
		require.NoError(t, err)

		archiveRoot := unmarshalArchiveRoot(t, *userData)
		require.Len(t, archiveRoot.Content, 2)

		content := mappingNodeValue(archiveRoot.Content[1], archiveEntryContent)
		require.NotNil(t, content, "the entry must carry the stripped content itself")
		assert.NotContains(t, content.Value, SitePhoneHomeName)
		assert.Contains(t, content.Value, "curl")
	})

	t.Run("keeps the content an entry of another type merges from a dropped one", func(t *testing.T) {
		// The merging entry is a script, so what it holds is text we must not
		// touch - even though the entry it merges is one we drop.
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- &base
  type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: `+phoneHomeURL+`
- <<: *base
  type: text/x-shellscript
`), phoneHomeURL)
		require.NoError(t, err)

		require.Len(t, unmarshalArchiveRoot(t, *userData).Content, 1, "only the cloud-config entry must go")

		// The text is kept, not run: the entry's own type says script, so what it
		// merges is a shell script that happens to read like cloud-config.
		assert.Contains(t, *userData, "url: "+phoneHomeURL,
			"the script entry keeps the text it merges")
	})

	t.Run("leaves alone an entry whose type merges to another format", func(t *testing.T) {
		// The merged entry is a script, so its cloud-config-looking content is
		// text we must not touch.
		const script = `#cloud-config-archive
- &base
  type: text/x-shellscript
  launch-index: 0
- <<: *base
  content: |
    #cloud-config
    phone_home:
      url: ` + phoneHomeURL + `
`

		userData, err := DisablePhoneHomeInUserData(new(script), phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, script, *userData)
	})

	t.Run("enabling does not leave the block it replaced", func(t *testing.T) {
		stale := strings.ReplaceAll(authored, phoneHomeURL, "http://old.example/phone-home")

		userData, err := EnablePhoneHomeInUserData(new(stale), phoneHomeURL)
		require.NoError(t, err)

		assert.NotContains(t, *userData, "old.example",
			"the block that was in there must not report anywhere once it is replaced")
		assert.Equal(t, 1, strings.Count(*userData, SitePhoneHomeName))
	})
}

func TestRemovePhoneHomeReadsTheLastOfADuplicatedKey(t *testing.T) {
	// yaml's loader keeps the last of a duplicated key, so that is the one the
	// instance reports to.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
packages:
- curl
phone_home:
  url: http://somebody.else/
  url: `+phoneHomeURL+`
`), phoneHomeURL)
	require.NoError(t, err)

	assert.Nil(t, mappingNodeValue(unmarshalDocumentRoot(t, *userData), SitePhoneHomeName))
}

func TestPhoneHomeMergedIntoADocument(t *testing.T) {
	// cloud-init reads a block merged in with `<<` as the mapping's own, so it is
	// taken out of the mapping it is written in - the only place it can be taken
	// out of, since a merged key cannot be overridden away.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	t.Run("through more than one merge key", func(t *testing.T) {
		// yaml applies every `<<` a mapping holds, so a block behind the first of
		// them is as live as one behind the last.
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
reporting: &reporting
  phone_home:
    url: `+phoneHomeURL+`
installed: &installed
  packages:
  - curl
autoinstall:
  version: 1
  user-data:
    <<: *reporting
    <<: *installed
`), phoneHomeURL)
		require.NoError(t, err)

		assert.NotContains(t, *userData, SitePhoneHomeName)
		assert.Contains(t, *userData, "curl", "what the other merge brings in must be kept")
	})

	userData, err := DisablePhoneHomeInUserData(new(`#cloud-config
defaults: &shared
  phone_home:
    url: `+phoneHomeURL+`
  packages:
  - curl
autoinstall:
  version: 1
  user-data:
    <<: *shared
`), phoneHomeURL)
	require.NoError(t, err)

	assert.NotContains(t, *userData, SitePhoneHomeName)
	assert.Contains(t, *userData, "curl", "the rest of what it merges must be kept")
}

func TestPhoneHomeInUserDataHoldingMoreThanOneDocument(t *testing.T) {
	// cloud-init reads one document out of user-data. Rendering back the one we
	// read would drop the rest, so more than one is not user-data we edit.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	const authored = `#cloud-config
packages:
- curl
phone_home:
  url: ` + phoneHomeURL + `
---
runcmd:
- [echo, hi]
`

	// Unsupported in both directions: the callers leave user-data alone on
	// disable, which is safe here because cloud-init's loader will not read this
	// document either, so the block in it never runs.
	_, err := DisablePhoneHomeInUserData(new(authored), phoneHomeURL)
	assert.ErrorIs(t, err, ErrUnsupportedUserData)

	_, err = EnablePhoneHomeInUserData(new(authored), phoneHomeURL)
	assert.ErrorIs(t, err, ErrUnsupportedUserData)
}

func TestRemovePhoneHomeKeepsWhatWasWrittenAroundContent(t *testing.T) {
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	// Stripped content is swapped in as a node of its own, so whatever yaml hung
	// on the node it replaces has to come across with it.
	userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: | # the base config
    #cloud-config
    packages:
    - curl
    phone_home:
      url: `+phoneHomeURL+`
`), phoneHomeURL)
	require.NoError(t, err)

	assert.Contains(t, *userData, "# the base config")
	assert.NotContains(t, *userData, SitePhoneHomeName)
}

func TestPhoneHomeInArchiveThatInstallsATargetSystem(t *testing.T) {
	// An autoinstall entry installs a system of its own, and phone-home belongs
	// in the config that system runs, not in the installer's. An entry of its own
	// would report from the installer, so the callback never comes from the
	// target.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	const authored = `#cloud-config-archive
- type: text/x-shellscript
  content: |
    #!/bin/sh
    echo hello
- type: text/cloud-config
  content: |
    #cloud-config
    autoinstall:
      version: 1
      user-data:
        packages:
        - curl
`

	t.Run("enabling puts it under the entry's autoinstall", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)

		archiveRoot := unmarshalArchiveRoot(t, *userData)
		require.Len(t, archiveRoot.Content, 2, "phone-home must not be added as an entry of its own")

		content := mappingNodeValue(archiveRoot.Content[1], archiveEntryContent)
		require.NotNil(t, content)

		installed := mappingNodeValue(
			mappingNodeValue(phoneHomeInContent(t, content.Value), autoinstallName), autoinstallUserData)
		require.NotNil(t, installed)
		assert.Equal(t, phoneHomeURL,
			mappingNodeValue(mappingNodeValue(installed, SitePhoneHomeName), SitePhoneHomeUrl).Value)
	})

	t.Run("passes over a templated entry", func(t *testing.T) {
		// yaml reads `{{ x }}` as a mapping and writes it back as one, so a jinja
		// entry cannot be rendered back and phone-home goes in an entry of its own.
		const templated = `#cloud-config-archive
- type: text/cloud-config
  content: |
    ## template: jinja
    #cloud-config
    autoinstall:
      version: 1
      user-data:
        hostname: {{ v1.local_hostname }}
`

		userData, err := EnablePhoneHomeInUserData(new(templated), phoneHomeURL)
		require.NoError(t, err)

		assert.Contains(t, *userData, "hostname: {{ v1.local_hostname }}",
			"the template must be left as authored")
		assert.Len(t, unmarshalArchiveRoot(t, *userData).Content, 2)
	})

	t.Run("reports an autoinstall it cannot reach into", func(t *testing.T) {
		// An entry of our own would report from the installer, not the system being
		// installed. The non-archive path rejects the same document, so this one
		// does too rather than report the wrong boot silently.
		for _, autoinstall := range []string{"not-a-mapping", "\n      version: 1\n      user-data: nope"} {
			_, err := EnablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    autoinstall: `+autoinstall+`
`), phoneHomeURL)
			require.ErrorContains(t, err, "must be a mapping to insert phone-home")
		}
	})

	t.Run("enabling twice does not duplicate it", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)

		userData, err = EnablePhoneHomeInUserData(userData, phoneHomeURL)
		require.NoError(t, err)

		assert.Equal(t, 1, strings.Count(*userData, SitePhoneHomeName))
	})

	t.Run("disabling takes it back out", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)

		userData, err = DisablePhoneHomeInUserData(userData, phoneHomeURL)
		require.NoError(t, err)

		// The entry is re-rendered, so yaml's indentation is what comes back, not
		// the authored text.
		assert.NotContains(t, *userData, SitePhoneHomeName)
		assert.Contains(t, *userData, autoinstallName, "the rest of the entry must be kept")
		assert.Contains(t, *userData, "curl")
	})
}

func TestPhoneHomeTogglesInAnArchive(t *testing.T) {
	// Unchecking the box and checking it again is the flow the UI drives, so what
	// each direction stores has to be user-data the other can read back.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	t.Run("enabling, disabling and enabling again", func(t *testing.T) {
		const authored = `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    packages:
    - curl
`

		userData, err := EnablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)

		userData, err = DisablePhoneHomeInUserData(userData, phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, authored, *userData, "disabling must give back the archive as authored")

		userData, err = EnablePhoneHomeInUserData(userData, phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, 1, strings.Count(*userData, SitePhoneHomeName))
		assert.Contains(t, *userData, "curl", "the entries around it must survive the toggling")
	})

	t.Run("enabling on an archive that disabling emptied", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: `+phoneHomeURL+`
`), phoneHomeURL)
		require.NoError(t, err)
		require.Empty(t, unmarshalArchiveRoot(t, *userData).Content, "an emptied archive keeps its header")

		userData, err = EnablePhoneHomeInUserData(userData, phoneHomeURL)
		require.NoError(t, err)

		assert.Equal(t, `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      post: all
      url: `+phoneHomeURL+`
`, *userData, "the entry must be written out, not in the flow style `[]` reads back as")
	})
}

func TestPhoneHomeInAnEncodedArchiveEntry(t *testing.T) {
	// An entry can carry its content encoded, which is not user-data we can read:
	// it is left exactly as authored in both directions. Phone-home inside it is
	// therefore not ours to take out, and enabling adds an entry of our own
	// rather than rewriting theirs.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	encoded := base64.StdEncoding.EncodeToString(
		[]byte("#cloud-config\nphone_home:\n  url: " + phoneHomeURL + "\n"))
	authored := `#cloud-config-archive
- type: text/cloud-config
  encoding: b64
  content: ` + encoded + "\n"

	t.Run("disabling leaves it as authored", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, authored, *userData)
	})

	t.Run("enabling adds an entry of its own", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(authored), phoneHomeURL)
		require.NoError(t, err)

		require.Len(t, unmarshalArchiveRoot(t, *userData).Content, 2)
		assert.Contains(t, *userData, encoded, "the encoded entry must be left as authored")
	})
}

func TestPhoneHomeInScalarArchiveEntry(t *testing.T) {
	// cloud-init reads a scalar entry as the content of an entry with no type,
	// so phone-home in one is live and must be handled like any other entry.
	const phoneHomeURL = "http://169.254.169.254/phone-home"

	archive := `#cloud-config-archive
- |
  #cloud-config
  phone_home:
    url: ` + phoneHomeURL + `
- |
  #cloud-config
  packages:
  - curl
`

	t.Run("disabling removes it", func(t *testing.T) {
		userData, err := DisablePhoneHomeInUserData(new(archive), phoneHomeURL)
		require.NoError(t, err)

		require.Len(t, unmarshalArchiveRoot(t, *userData).Content, 1, "the scalar entry must be removed")
		assert.NotContains(t, *userData, SitePhoneHomeName)
		assert.Contains(t, *userData, "curl", "unrelated entries must be kept")
	})

	t.Run("enabling replaces it instead of duplicating it", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new(archive), phoneHomeURL)
		require.NoError(t, err)

		assert.Equal(t, 1, strings.Count(*userData, SitePhoneHomeName), "exactly one phone-home must remain")
		assert.Contains(t, *userData, phoneHomeURL)
	})
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
`), nil, "")
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
		// User-data that declares no format is taken as #cloud-config, and
		// enabling writes that marker so cloud-init reads the document at all.
		{"a mapping with no header", "packages:\n- curl\n", true},
		{"empty mapping", "{}\n", true},
		{"#cloud-config-archive", "#cloud-config-archive\n- type: text/cloud-config\n  content: x\n", true},
		{"empty #cloud-config-archive", "#cloud-config-archive\n[]\n", true},
		{"header-less list is not an archive", "- type: text/cloud-config\n  content: x\n", false},
		// A comment declares no format either, so the same applies.
		{"a comment above the header", "# a note\n#cloud-config\npackages:\n- curl\n", true},
		{"a comment instead of a header", "# my notes\npackages:\n- curl\n", true},
		// A format of its own that cloud-init dispatches elsewhere: a jsonp patch
		// goes to its jsonpatch handler, not the yaml merger.
		{"#cloud-config-jsonp", "#cloud-config-jsonp\n{\"op\": \"add\", \"path\": \"/a\", \"value\": 1}\n", false},
		{"#cloud-boothook", "#cloud-boothook\npackages:\n- curl\n", false},
		// yaml reads past a space or a newline before the header, but not a tab:
		// the document below one cannot be read at all.
		{"a header behind a tab", "\t#cloud-config\npackages:\n- curl\n", false},
		{"#!/bin/bash script parsed as a scalar", "#!/bin/bash\necho hello\nls -la\n", false},
		{"#!/bin/bash script parsed as a mapping", "#!/bin/bash\nexport FOO: bar\n", false},
		// A template is not a document to render back, whatever it declares below
		// the marker: yaml reads `{{ x }}` as a mapping and writes it back as one.
		{"jinja #cloud-config", "## template: jinja\n#cloud-config\npackages:\n- curl\n", false},
		// cloud-init would not run this one at all: its jinja handler dispatches
		// cloud-config, scripts and boothooks only, never an archive.
		{
			"jinja #cloud-config-archive",
			"## template: jinja\n#cloud-config-archive\n- type: text/cloud-config\n  content: x\n",
			false,
		},
		// cloud-init matches the marker on the start of the line, ignoring case.
		{"#cloud-config with a note after it", "#cloud-config (managed by nico)\npackages:\n- curl\n", true},
		// cloud-init reads past whatever follows the marker, space or not.
		{"#cloud-config with a tab before the note", "#cloud-config\t(managed by nico)\npackages:\n- curl\n", true},
		{"#cloud-configuration", "#cloud-configuration\npackages:\n- curl\n", true},
		{"#Cloud-Config", "#Cloud-Config\npackages:\n- curl\n", true},
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
	// is exactly what a removal can take away.
	tests := []struct {
		name       string
		userData   string
		wantHeader string
	}{
		{
			name:       "#cloud-config carrying the header on the phone-home key",
			userData:   "#cloud-config\nphone_home:\n  url: " + phoneHomeURL + "\npackages:\n- curl\n",
			wantHeader: "#cloud-config\n",
		},
		{
			// cloud-init reads the header past leading whitespace, so an archive
			// written behind a blank line is one we edit like any other - and the
			// blank line is the author's, so it is stored back with it.
			name:       "#cloud-config-archive behind a blank line",
			userData:   "\n#cloud-config-archive\n- type: text/cloud-config\n  content: |\n    #cloud-config\n    package_update: true\n    phone_home:\n      url: " + phoneHomeURL + "\n",
			wantHeader: "\n#cloud-config-archive\n",
		},
		{
			name: "#cloud-config-archive carrying the header on the phone-home entry",
			userData: `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: ` + phoneHomeURL + `
- type: text/cloud-config
  content: |
    #cloud-config
    packages:
    - curl
`,
			wantHeader: "#cloud-config-archive\n",
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
			// The marker is the header line; what it declares below is body, and a
			// template is reported rather than edited anyway.
			"the jinja marker",
			"## template: jinja\n#cloud-config\npackages: []\n",
			"## template: jinja\n",
			"#cloud-config\npackages: []\n",
		},
		{
			// cloud-init matches the header past leading whitespace, which is kept
			// with the line so the document is stored as its author wrote it.
			"a header behind whitespace",
			"\n  #cloud-config\npackages: []\n",
			"\n  #cloud-config\n",
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
			assert.Equal(t, tt.userData, header+body, "the split must not lose a byte")
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

func phoneHomeInContent(t *testing.T, content string) *yaml.Node {
	t.Helper()

	inner := &yaml.Node{}
	require.NoError(t, yaml.Unmarshal([]byte(content), inner))
	require.Len(t, inner.Content, 1)

	return inner.Content[0]
}

func phoneHomeFromContent(t *testing.T, content string) *yaml.Node {
	t.Helper()

	return mappingNodeValue(phoneHomeInContent(t, content), SitePhoneHomeName)
}

func TestRenderedUserDataKeepsItsIndent(t *testing.T) {
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

	t.Run("an appended archive entry is written at the same indent", func(t *testing.T) {
		userData, err := EnablePhoneHomeInUserData(new("#cloud-config-archive\n- |\n  #!/bin/sh\n  echo hi\n"), phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, `#cloud-config-archive
- |
  #!/bin/sh
  echo hi
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      post: all
      url: `+phoneHomeURL+`
`, *userData)
	})

	t.Run("an entry rewritten through its text is written out, not in flow", func(t *testing.T) {
		// Stripping the block empties the entry's autoinstall user-data, which
		// renders as `{}` and reads back flow-styled. What goes in it is written
		// out, so re-enabling stores what the author would have written.
		userData, err := EnablePhoneHomeInUserData(new(`#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    autoinstall:
      version: 1
      user-data:
        phone_home:
          url: http://stale/ph
`), phoneHomeURL)
		require.NoError(t, err)
		assert.Equal(t, `#cloud-config-archive
- type: text/cloud-config
  content: |
    #cloud-config
    autoinstall:
      version: 1
      user-data:
        phone_home:
          post: all
          url: `+phoneHomeURL+`
`, *userData)
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
