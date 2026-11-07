// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"os"
	"text/tabwriter"

	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var ruleListCmd = &cobra.Command{
	Use:   "list",
	Short: "List operation rules",
	Long:  `List all operation rules with optional filtering by operation type or default status.`,
	RunE:  runRuleList,
}

var (
	listOperationType string
	listIsDefault     bool
	listOffset        int32
	listLimit         int32
)

func init() {
	ruleCmd.AddCommand(ruleListCmd)

	ruleListCmd.Flags().StringVar(&listOperationType, "operation-type", "", "Filter by operation type (power_control, firmware_control)")
	ruleListCmd.Flags().BoolVar(&listIsDefault, "default-only", false, "Show only default rules")
	ruleListCmd.Flags().Int32Var(&listOffset, "offset", 0, "Pagination offset")
	ruleListCmd.Flags().Int32Var(&listLimit, "limit", 100, "Pagination limit")
}

// runRuleList is the RunE handler for ruleListCmd. It calls ListOperationRules
// with any provided filters and prints the results as a tab-aligned table.
func runRuleList(cmd *cobra.Command, args []string) error {
	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		return fmt.Errorf("failed to create client: %w", err)
	}
	defer flowClient.Close()

	var opType *types.OperationType
	var isDefault *bool
	offset := int(listOffset)
	limit := int(listLimit)

	if listIsDefault {
		isDefaultVal := true
		isDefault = &isDefaultVal
	}

	if listOperationType != "" {
		var opTypeVal types.OperationType
		switch listOperationType {
		case "power_control":
			opTypeVal = types.OperationTypePowerControl
		case "firmware_control":
			opTypeVal = types.OperationTypeFirmwareControl
		default:
			return fmt.Errorf("invalid operation type: %s (must be power_control or firmware_control)", listOperationType)
		}
		opType = &opTypeVal
	}

	rules, totalCount, err := flowClient.ListOperationRules(
		context.Background(),
		opType,
		isDefault,
		&offset,
		&limit,
	)
	if err != nil {
		return fmt.Errorf("failed to list rules: %w", err)
	}

	if len(rules) == 0 {
		fmt.Println("No rules found")
		return nil
	}

	// Print in table format
	w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
	fmt.Fprintln(w, "ID\tNAME\tOPERATION TYPE\tOPERATION CODE\tDEFAULT\tCREATED AT")
	fmt.Fprintln(w, "--\t----\t--------------\t--------------\t-------\t----------")

	for _, rule := range rules {
		opTypeStr := ""
		switch rule.OperationType {
		case types.OperationTypePowerControl:
			opTypeStr = "power_control"
		case types.OperationTypeFirmwareControl:
			opTypeStr = "firmware_control"
		default:
			opTypeStr = "unknown"
		}

		isDefaultStr := "no"
		if rule.IsDefault {
			isDefaultStr = "yes"
		}

		createdAt := ""
		if !rule.CreatedAt.IsZero() {
			createdAt = rule.CreatedAt.Format("2006-01-02 15:04:05")
		}

		fmt.Fprintf(w, "%s\t%s\t%s\t%s\t%s\t%s\n",
			rule.ID.String(),
			rule.Name,
			opTypeStr,
			rule.OperationCode,
			isDefaultStr,
			createdAt,
		)
	}

	w.Flush()
	fmt.Printf("\nTotal: %d rules\n", totalCount)

	return nil
}
