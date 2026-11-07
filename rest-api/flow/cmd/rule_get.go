// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/google/uuid"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var ruleGetCmd = &cobra.Command{
	Use:   "get <rule-id>",
	Short: "Get operation rule details",
	Long:  `Get detailed information about a specific operation rule by ID.`,
	Args:  cobra.ExactArgs(1),
	RunE:  runRuleGet,
}

func init() {
	ruleCmd.AddCommand(ruleGetCmd)
}

// runRuleGet is the RunE handler for ruleGetCmd. It parses the rule ID from
// the positional argument, fetches the rule via the client, and prints its
// details to stdout.
func runRuleGet(cmd *cobra.Command, args []string) error {
	ruleIDStr := args[0]

	ruleID, err := uuid.Parse(ruleIDStr)
	if err != nil {
		return fmt.Errorf("invalid rule ID: %w", err)
	}

	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		return fmt.Errorf("failed to create client: %w", err)
	}
	defer flowClient.Close()

	rule, err := flowClient.GetOperationRule(context.Background(), ruleID)
	if err != nil {
		return fmt.Errorf("failed to get rule: %w", err)
	}

	opTypeStr := ""
	switch rule.OperationType {
	case types.OperationTypePowerControl:
		opTypeStr = "power_control"
	case types.OperationTypeFirmwareControl:
		opTypeStr = "firmware_control"
	default:
		opTypeStr = "unknown"
	}

	fmt.Printf("ID:              %s\n", rule.ID.String())
	fmt.Printf("Name:            %s\n", rule.Name)
	fmt.Printf("Description:     %s\n", rule.Description)
	fmt.Printf("Operation Type:  %s\n", opTypeStr)
	fmt.Printf("Operation Code:  %s\n", rule.OperationCode)
	fmt.Printf("Is Default:      %t\n", rule.IsDefault)

	if !rule.CreatedAt.IsZero() {
		fmt.Printf("Created At:      %s\n", rule.CreatedAt.Format("2006-01-02 15:04:05"))
	}
	if !rule.UpdatedAt.IsZero() {
		fmt.Printf("Updated At:      %s\n", rule.UpdatedAt.Format("2006-01-02 15:04:05"))
	}

	fmt.Println("\nRule Definition (JSON):")
	// Pretty print the JSON
	var prettyJSON map[string]interface{}
	if err := json.Unmarshal([]byte(rule.RuleDefinitionJSON), &prettyJSON); err == nil {
		formatted, _ := json.MarshalIndent(prettyJSON, "", "  ")
		fmt.Println(string(formatted))
	} else {
		fmt.Println(rule.RuleDefinitionJSON)
	}

	return nil
}
