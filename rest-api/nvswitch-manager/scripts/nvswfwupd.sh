#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -uo pipefail     # drop -e so loop keeps running even if one poll fails

BMC_IP="${1:-}"
BMC_USER="${2:-}"
BMC_PASS="${3:-}"
FW_PKG="${4:-}"

POLL_INTERVAL=10   # seconds
DOT_INTERVAL=6     # add one ".....X%" chunk every N polls
MAX_LEN=132

############################################
# Argument and basic checks
############################################

if [[ -z "$BMC_IP" || -z "$BMC_USER" || -z "$BMC_PASS" || -z "$FW_PKG" ]]; then
  echo "Usage: $0 <BMC_IP> <BMC_USER> <BMC_PASS> <FW_PKG>"
  exit 1
fi

if [[ ! -f "$FW_PKG" ]]; then
  echo "Firmware package '$FW_PKG' not found"
  exit 1
fi

if ! ping -W 1 -c 1 "$BMC_IP" &>/dev/null; then
  echo "BMC $BMC_IP is not reachable"
  exit 1
fi

echo "BMC IP      : $BMC_IP"
echo "BMC user    : $BMC_USER"
echo "FW package  : $FW_PKG"
echo

############################################
# Firmware upload (returns Task URI)
############################################

echo "Uploading firmware to BMC..."

resp_json="$(mktemp)"
# IPv6 hosts need brackets in URLs; ping uses the original address.
URL_HOST="$BMC_IP"
if [[ "$URL_HOST" == *:* && "$URL_HOST" != \[*\] ]]; then
  URL_HOST="[$URL_HOST]"
fi

if ! curl -ksu "${BMC_USER}:${BMC_PASS}" \
      -H "Content-Type:application/octet-stream" \
      -X POST "https://${URL_HOST}/redfish/v1/UpdateService" \
      -T "${FW_PKG}" \
      -o "$resp_json"; then
  echo "Upload POST failed"
  rm -f "$resp_json"
  exit 1
fi

echo "Upload request sent, parsing task information..."

task_uri="$(
  grep -E 'TaskService/Tasks' "$resp_json" \
    | head -n1 \
    | sed -E 's/.*"(\/redfish\/v1\/TaskService\/Tasks\/[^"]*)".*/\1/'
)"

rm -f "$resp_json"

if [[ -z "$task_uri" ]]; then
  echo "Could not find Task URI in response; check BMC response manually."
  exit 1
fi

echo "Task URI: ${task_uri}"
echo

############################################
# Poll task every 10 seconds with progress
############################################

poll_count=0
state="Running"
status="OK"
pct="0"

base_prefix="Task state=${state}, status=${status}, ${pct}%"
printf "%s" "$base_prefix"
current_len=${#base_prefix}

while true; do
  sleep "$POLL_INTERVAL"
  ((poll_count++))

  task_json="$(mktemp)"
  if ! curl -ksu "${BMC_USER}:${BMC_PASS}" \
        -X GET "https://${URL_HOST}${task_uri}" \
        -o "$task_json"; then
    # On transient failure, just note it and continue polling
    rm -f "$task_json"
    printf "\nWarning: failed to poll task, will retry...\n"
    current_len=0
    continue
  fi

  # Default values in case parsing fails
  new_state="$(grep -oE '"TaskState"\s*:\s*"[^"]+"' "$task_json" | sed -E 's/.*"TaskState"\s*:\s*"([^"]*)".*/\1/' || true)"
  new_status="$(grep -oE '"TaskStatus"\s*:\s*"[^"]+"' "$task_json" | sed -E 's/.*"TaskStatus"\s*:\s*"([^"]*)".*/\1/' || true)"
  new_pct="$(grep -oE '"PercentComplete"\s*:\s*[0-9]+' "$task_json" | sed -E 's/.*"PercentComplete"\s*:\s*([0-9]+).*/\1/' || true)"
  rm -f "$task_json"

  [[ -n "$new_state"  ]] && state="$new_state"
  [[ -n "$new_status" ]] && status="$new_status"
  [[ -n "$new_pct"    ]] && pct="$new_pct"
  [[ -z "$pct"        ]] && pct="0"

  # Every DOT_INTERVAL polls, append "<pct>%"
  if (( poll_count % DOT_INTERVAL == 0 )); then
    chunk="${pct}%"
    chunk_len=${#chunk}

    # If adding this chunk exceeds MAX_LEN, start a new line
    if (( current_len + chunk_len > MAX_LEN )); then
      printf "\n"
      current_len=0
    fi

    printf "%s" "$chunk"
    (( current_len += chunk_len ))
  else
    chunk="."
    chunk_len=${#chunk}

    # If adding this chunk exceeds MAX_LEN, start a new line
    if (( current_len + chunk_len > MAX_LEN )); then
      printf "\n"
      current_len=0
    fi

    printf "%s" "$chunk"
    (( current_len += chunk_len ))
  fi

  # Success
  if [[ "$state" == "Completed" && "$pct" -ge 100 ]]; then
    echo -n "${pct}%"
    printf "\nstate=%s, status=%s, percent=%s%%\n" "$state" "$status" "$pct"
    printf "\nFirmware update task completed successfully\n"
  exit 0
  fi

  # Failure / error states
  if [[ "$state" == "Exception" || "$state" == "Killed" || "$status" == "Critical" || "$status" == "Warning" ]]; then
    echo -n "${pct}%"
    printf "\nstate=%s, status=%s, percent=%s%%\n" "$state" "$status" "$pct"
    printf "\nFirmware update task failed\n"
    exit 1
  fi
done
