// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package config reads and checks /etc/scale-test/config.yaml. Every rule
// lives here, so a hand-made Job cannot skip one.
package config

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"path"
	"regexp"
	"slices"
	"strconv"
	"strings"

	"gopkg.in/yaml.v3"
)

// Limits. The upper bounds keep later duration math from overflowing.
const (
	runIDMaxLen           = 40
	resultMaxBytesMin     = 262_144
	resultMaxBytesMax     = 1_000_000
	maxNameBytes          = 253 // a pod name, and the journal claim
	maxJournalDirBytes    = 512
	maxProbeTimeoutSecs   = 600
	maxProbePollSecs      = 3_600
	maxProbeContactSecs   = 86_400
	maxWholeRunSecs       = 7 * 86_400
	defaultResultMaxBytes = 900_000
)

var (
	dnsLabel = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]{0,61}[a-z0-9])?$`)
	yamlLine = regexp.MustCompile(`line \d+`)
)

const (
	subdomain  = "must be a DNS subdomain name"
	yamlReason = "unknown key, wrong value type or bad YAML"
)

// textKeys are the keys that hold text. Every other scalar outside settings
// is an integer, or a boolean for dryRun.
var textKeys = []string{"runId", "command", "namespace", "executionId", "lockName", "url", "labelSelector", "dir", "claim", "run", "control", "result"}

// required are the keys a present mapping must hold, as serde needs them.
var required = map[string]string{"site": "lockName", "journal": "dir"}

// loose reports a value yaml.v3 would bend into its field: yes or on as a
// boolean, 5.5 or 2^64 as an integer, an alias, a null other than dryRun,
// or a site or journal mapping without its required key. serde and the
// chart refuse them.
func loose(key string, n *yaml.Node) bool {
	switch n.Kind {
	case yaml.DocumentNode, yaml.SequenceNode:
		return slices.ContainsFunc(n.Content, func(c *yaml.Node) bool { return loose(key, c) })
	case yaml.MappingNode:
		keys := []string{}
		for i := 0; i+1 < len(n.Content); i += 2 {
			if k := n.Content[i].Value; k != "settings" && loose(k, n.Content[i+1]) {
				return true
			}
			keys = append(keys, n.Content[i].Value)
		}
		return required[key] != "" && !slices.Contains(keys, required[key])
	case yaml.ScalarNode:
		if n.ShortTag() == "!!null" {
			return key != "dryRun"
		}
		want := "!!int"
		if key == "dryRun" {
			want = "!!bool"
		}
		return !slices.Contains(textKeys, key) && n.ShortTag() != want
	case yaml.AliasNode:
		return true
	}
	return false
}

// Error is a refused config. Field names the key. Reason is fixed text: the
// run ID and the command are the only values it quotes, and no YAML text or
// secret ever reaches it.
type Error struct{ Field, Reason string }

func (e *Error) Error() string { return "config " + e.Field + ": " + e.Reason }

// Config is the run config. Unknown keys are errors at every level.
type Config struct {
	RunID          string        `yaml:"runId"`
	Command        string        `yaml:"command"`
	DryRun         *bool         `yaml:"dryRun"`
	Namespace      string        `yaml:"namespace"`
	ExecutionID    string        `yaml:"executionId"` // main sets it from SCALE_TEST_EXECUTION_ID
	Site           Site          `yaml:"site"`
	Probe          Probe         `yaml:"probe"`
	Timeouts       Timeouts      `yaml:"timeouts"`
	ControlPlane   []PodSelector `yaml:"controlPlane"`
	Journal        Journal       `yaml:"journal"`
	ConfigMaps     ConfigMaps    `yaml:"configMaps"`
	ResultMaxBytes uint32        `yaml:"resultMaxBytes"`
	// Settings is the command's own mapping, read by the command.
	Settings yaml.Node `yaml:"settings"`
}

type Site struct {
	LockName string `yaml:"lockName"`
}

type Probe struct {
	URL                   string `yaml:"url"`
	RequestTimeoutSeconds uint64 `yaml:"requestTimeoutSeconds"`
	PollSeconds           uint64 `yaml:"pollSeconds"`
	ContactSeconds        uint64 `yaml:"contactSeconds"`
	MaxContactGapSeconds  uint64 `yaml:"maxContactGapSeconds"`
}

type Timeouts struct {
	WholeRunSeconds  uint64 `yaml:"wholeRunSeconds"`
	ReadinessSeconds uint64 `yaml:"readinessSeconds"`
	FinalizeSeconds  uint64 `yaml:"finalizeSeconds"`
}

type PodSelector struct {
	Namespace     string `yaml:"namespace"`
	LabelSelector string `yaml:"labelSelector"`
}

type Journal struct {
	Dir   string `yaml:"dir"`
	Claim string `yaml:"claim"`
}

type ConfigMaps struct {
	Run    string `yaml:"run"`
	Result string `yaml:"result"`
}

// Load reads path, fills the defaults and checks every rule for command.
func Load(path, command string) (*Config, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read config: %w", err)
	}
	return Parse(raw, command)
}

// Parse is Load without the file read.
func Parse(raw []byte, command string) (*Config, error) {
	c := &Config{
		Site:           Site{LockName: "nico-scale-test-site-lock"},
		Probe:          Probe{URL: "http://nico-site-health-probe-api:9010", RequestTimeoutSeconds: 5, PollSeconds: 15, ContactSeconds: 60, MaxContactGapSeconds: 300},
		Timeouts:       Timeouts{ReadinessSeconds: 300, FinalizeSeconds: 900},
		Journal:        Journal{Dir: "/journal"},
		ResultMaxBytes: defaultResultMaxBytes,
	}
	dec := yaml.NewDecoder(bytes.NewReader(raw))
	dec.KnownFields(true)
	err := dec.Decode(c)
	if err == nil && dec.Decode(&yaml.Node{}) != io.EOF {
		err = errors.New("more than one document")
	}
	if err != nil {
		reason := yamlReason
		if l := yamlLine.FindString(err.Error()); l != "" {
			reason += " at " + l
		}
		return nil, &Error{"yaml", reason}
	}
	var doc yaml.Node
	_ = yaml.Unmarshal(raw, &doc)
	if loose("", &doc) {
		return nil, &Error{"yaml", yamlReason}
	}
	for name, suffix := range map[*string]string{&c.ConfigMaps.Run: "run", &c.ConfigMaps.Result: "result"} {
		if *name == "" {
			*name = c.RunID + "-" + suffix
		}
	}
	if c.Settings.IsZero() || c.Settings.ShortTag() == "!!null" {
		c.Settings = yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
	}
	return c, c.validate(command)
}

func isDNSSubdomain(s string) bool {
	return len(s) <= maxNameBytes && !slices.ContainsFunc(strings.Split(s, "."), func(l string) bool { return !dnsLabel.MatchString(l) })
}

// probeURLReason is why raw cannot be the probe URL, or "". The client puts
// /v1/... at the root, so a path would be dropped, not kept.
func probeURLReason(raw string) string {
	u, err := url.Parse(raw)
	if err == nil && u.Port() != "" {
		_, err = strconv.ParseUint(u.Port(), 10, 16)
	}
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" {
		return "must be an http or https URL"
	}
	switch {
	case u.User != nil:
		return "must not hold a user name or password"
	case strings.ContainsAny(raw, "?#"):
		return "must not hold a query or fragment"
	case u.Path != "" && u.Path != "/":
		return "must not hold a path"
	}
	return ""
}

// validate runs every rule that needs no knowledge of the command's writes,
// in the Rust order. The first refusal wins.
func (c *Config) validate(command string) error {
	var err error
	rule := func(ok bool, field, reason string) {
		if !ok && err == nil {
			err = &Error{field, reason}
		}
	}
	id := c.RunID
	rule(id != "" && len(id) <= runIDMaxLen, "runId", fmt.Sprintf("%.64q must have 1 to 40 characters", id))
	rule(strings.Trim(id, "abcdefghijklmnopqrstuvwxyz0123456789-") == "", "runId", fmt.Sprintf("%.64q may hold only a-z, 0-9 and '-'", id))
	rule(!strings.HasPrefix(id, "-") && !strings.HasSuffix(id, "-"), "runId", fmt.Sprintf("%.64q must start and end with a letter or digit", id))
	rule(c.Command == command, "command", fmt.Sprintf("%.64q does not match the CLI command %q", c.Command, command))
	rule(dnsLabel.MatchString(c.Namespace), "namespace", "must be a DNS label")
	rule(isDNSSubdomain(c.Site.LockName), "site.lockName", subdomain)
	m := c.ConfigMaps
	rule(isDNSSubdomain(m.Run), "configMaps.run", subdomain)
	rule(isDNSSubdomain(m.Result), "configMaps.result", subdomain)
	rule(m.Run != m.Result, "configMaps", "run and result must be different names")

	p := c.Probe
	reason := probeURLReason(p.URL)
	rule(reason == "", "probe.url", reason)
	rule(p.RequestTimeoutSeconds > 0, "probe.requestTimeoutSeconds", "must be over 0")
	rule(p.RequestTimeoutSeconds <= maxProbeTimeoutSecs, "probe.requestTimeoutSeconds", fmt.Sprintf("must be at most %d", maxProbeTimeoutSecs))
	rule(p.PollSeconds > 0, "probe.pollSeconds", "must be over 0")
	rule(p.PollSeconds <= maxProbePollSecs, "probe.pollSeconds", fmt.Sprintf("must be at most %d", maxProbePollSecs))
	rule(p.ContactSeconds >= p.PollSeconds, "probe.contactSeconds", "must be at least probe.pollSeconds")
	rule(p.MaxContactGapSeconds >= p.ContactSeconds, "probe.maxContactGapSeconds", "must be at least probe.contactSeconds")
	rule(p.MaxContactGapSeconds <= maxProbeContactSecs, "probe.maxContactGapSeconds", fmt.Sprintf("must be at most %d", maxProbeContactSecs))

	t := c.Timeouts
	rule(t.ReadinessSeconds > 0, "timeouts.readinessSeconds", "must be over 0")
	rule(t.ReadinessSeconds < t.FinalizeSeconds && t.FinalizeSeconds < t.WholeRunSeconds, "timeouts", "need readinessSeconds < finalizeSeconds < wholeRunSeconds")
	rule(t.WholeRunSeconds <= maxWholeRunSecs, "timeouts.wholeRunSeconds", fmt.Sprintf("must be at most %d", maxWholeRunSecs))

	for i, s := range c.ControlPlane {
		rule(dnsLabel.MatchString(s.Namespace), fmt.Sprintf("controlPlane[%d].namespace", i), "must be a DNS label")
		rule(strings.TrimSpace(s.LabelSelector) != "", fmt.Sprintf("controlPlane[%d].labelSelector", i), "must not be empty")
	}
	// Dir and claim end up in every result, also the bounded terminal one.
	rule(path.IsAbs(c.Journal.Dir), "journal.dir", "must be an absolute path")
	rule(len(c.Journal.Dir) <= maxJournalDirBytes, "journal.dir", fmt.Sprintf("must be at most %d bytes", maxJournalDirBytes))
	rule(len(c.Journal.Claim) <= maxNameBytes, "journal.claim", fmt.Sprintf("must be at most %d bytes", maxNameBytes))
	rule(c.ResultMaxBytes >= resultMaxBytesMin && c.ResultMaxBytes <= resultMaxBytesMax, "resultMaxBytes", fmt.Sprintf("must be %d to %d", resultMaxBytesMin, resultMaxBytesMax))
	rule(c.Settings.Kind == yaml.MappingNode, "settings", "must be a mapping")
	return err
}

// CheckDryRun requires dryRun for a command that writes and refuses it for
// one that does not.
func (c *Config) CheckDryRun(writes bool) error {
	if writes && c.DryRun == nil {
		return &Error{"dryRun", "is required for a command that writes (true or false)"}
	}
	if !writes && c.DryRun != nil {
		return &Error{"dryRun", "must be missing for a command that makes no writes"}
	}
	return nil
}

// CheckExecutionID requires the pod name, which must be known before the run
// takes the lock.
func (c *Config) CheckExecutionID() error {
	if c.ExecutionID == "" {
		return &Error{"executionId", "is empty (set SCALE_TEST_EXECUTION_ID to the pod name)"}
	}
	if len(c.ExecutionID) > maxNameBytes {
		return &Error{"executionId", fmt.Sprintf("must be at most %d bytes, like a pod name", maxNameBytes)}
	}
	return nil
}
