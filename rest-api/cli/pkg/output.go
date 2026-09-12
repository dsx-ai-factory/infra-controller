// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"text/tabwriter"

	cli "github.com/urfave/cli/v2"
	"gopkg.in/yaml.v3"
)

// allowedOutputFormats is the canonical, ordered set of values the --output
// flag accepts. Kept as a slice (not a map) so error messages can render the
// list deterministically.
var allowedOutputFormats = []string{"json", "yaml", "table"}

// tableColumnSpec defines a single table column with a header and a value extractor.
type tableColumnSpec struct {
	Header string
	Get    func(item map[string]any) string
}

// tableColumnsByOperation maps OpenAPI operation IDs to their custom table columns.
// Falls back to generic tableFields for unregistered operations.
var tableColumnsByOperation = map[string][]tableColumnSpec{
	"get-all-vpc-peering": {
		{Header: "ID", Get: func(i map[string]any) string { return nestedString(i, "id") }},
		{Header: "VPC1 Name", Get: func(i map[string]any) string { return nestedString(i, "vpc1", "name") }},
		{Header: "VPC1 ID", Get: func(i map[string]any) string { return nestedString(i, "vpc1", "id") }},
		{Header: "VPC2 Name", Get: func(i map[string]any) string { return nestedString(i, "vpc2", "name") }},
		{Header: "VPC2 ID", Get: func(i map[string]any) string { return nestedString(i, "vpc2", "id") }},
	},
	"get-vpc-peering": {
		{Header: "ID", Get: func(i map[string]any) string { return nestedString(i, "id") }},
		{Header: "VPC1 Name", Get: func(i map[string]any) string { return nestedString(i, "vpc1", "name") }},
		{Header: "VPC1 ID", Get: func(i map[string]any) string { return nestedString(i, "vpc1", "id") }},
		{Header: "VPC2 Name", Get: func(i map[string]any) string { return nestedString(i, "vpc2", "name") }},
		{Header: "VPC2 ID", Get: func(i map[string]any) string { return nestedString(i, "vpc2", "id") }},
	},
}

// nestedString safely retrieves a nested string value using dot-separated path.
// Returns empty string if any level is missing or not a map.
func nestedString(item map[string]any, path ...string) string {
	current := any(item)
	for _, key := range path {
		m, ok := current.(map[string]any)
		if !ok {
			return ""
		}
		v, ok := m[key]
		if !ok {
			return ""
		}
		current = v
	}
	if s, ok := current.(string); ok {
		return s
	}
	return ""
}

// ValidateOutputFormat returns an error if format is outside the allowed set.
// The empty string is treated as valid so the StringFlag default ("json") and
// callers that pass an unset value continue to work.
//
// Without this validator, FormatOutput silently routed any unknown value to
// formatJSON, so a typo like `--output xml` exited 0 and produced JSON --
// dangerous in scripts that branch on the requested format.
func ValidateOutputFormat(format string) error {
	if format == "" {
		return nil
	}
	for _, allowed := range allowedOutputFormats {
		if format == allowed {
			return nil
		}
	}
	return fmt.Errorf(
		"invalid value %q for flag --output: allowed values are %s",
		format,
		strings.Join(allowedOutputFormats, ", "),
	)
}

// validateOutputFlag is the urfave/cli StringFlag.Action callback. It runs
// after the flag value is parsed and returns the same error as
// ValidateOutputFormat so an invalid --output value fails before any auth or
// HTTP work in the command Action.
func validateOutputFlag(_ *cli.Context, value string) error {
	return ValidateOutputFormat(value)
}

func FormatOutput(data []byte, format string) error {
	return FormatOutputWithOperation(data, format, "")
}

func FormatOutputWithOperation(data []byte, format, operationID string) error {
	switch format {
	case "", "json":
		return formatJSON(data)
	case "yaml":
		return formatYAML(data)
	case "table":
		return formatTable(data, operationID)
	default:
		return ValidateOutputFormat(format)
	}
}

func formatJSON(data []byte) error {
	var v interface{}
	if err := json.Unmarshal(data, &v); err != nil {
		_, err = os.Stdout.Write(data)
		return err
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}

func formatYAML(data []byte) error {
	var v interface{}
	if err := json.Unmarshal(data, &v); err != nil {
		_, err = os.Stdout.Write(data)
		return err
	}
	return yaml.NewEncoder(os.Stdout).Encode(v)
}

var tableFields = []string{"id", "name", "status", "created", "updated"}

func formatTable(data []byte, operationID string) error {
	var raw interface{}
	if err := json.Unmarshal(data, &raw); err != nil {
		_, err = os.Stdout.Write(data)
		return err
	}

	var items []map[string]any
	switch v := raw.(type) {
	case []interface{}:
		for _, item := range v {
			if m, ok := item.(map[string]any); ok {
				items = append(items, m)
			}
		}
	case map[string]any:
		items = append(items, v)
	default:
		return formatJSON(data)
	}

	if len(items) == 0 {
		fmt.Println("(no results)")
		return nil
	}

	// Use custom columns for registered operations, else fall back to generic
	customCols := tableColumnsByOperation[operationID]
	var cols []tableColumnSpec
	if customCols != nil {
		cols = customCols
	} else {
		for _, f := range tableFields {
			if _, ok := items[0][f]; ok {
				field := f
				cols = append(cols, tableColumnSpec{
					Header: field,
					Get:    func(i map[string]any) string { return nestedString(i, field) },
				})
			}
		}
		if len(cols) == 0 {
			return formatJSON(data)
		}
	}

	w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
	for i, col := range cols {
		if i > 0 {
			fmt.Fprint(w, "\t")
		}
		fmt.Fprint(w, col.Header)
	}
	fmt.Fprintln(w)

	for _, item := range items {
		for i, col := range cols {
			if i > 0 {
				fmt.Fprint(w, "\t")
			}
			fmt.Fprintf(w, "%v", col.Get(item))
		}
		fmt.Fprintln(w)
	}

	return w.Flush()
}
