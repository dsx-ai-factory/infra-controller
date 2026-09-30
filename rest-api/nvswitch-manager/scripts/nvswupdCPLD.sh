#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -uo pipefail

SWITCH_IP="${1:-}"
USER="${2:-}"
PASS="${3:-}"
LOCAL_IMG="${4:-}"

if [[ -z "$SWITCH_IP" || -z "$USER" || -z "$PASS" || -z "$LOCAL_IMG" ]]; then
  echo "Usage: $0 <SWITCH_IP> <USER> <PASSWORD> <LOCAL_IMAGE_PATH>"
  exit 1
fi

if [[ ! -f "$LOCAL_IMG" ]]; then
  echo "Image file '$LOCAL_IMG' not found"
  exit 1
fi

REMOTE_IMG="$(basename "$LOCAL_IMG")"
REMOTE_PATH="/home/${USER}/${REMOTE_IMG}"

SSHPASS_BASE=(sshpass -p "$PASS")
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)

# SCP needs brackets around IPv6 hosts; SSH and ping use the original address.
SCP_HOST="$SWITCH_IP"
if [[ "$SCP_HOST" == *:* && "$SCP_HOST" != \[*\] ]]; then
  SCP_HOST="[$SCP_HOST]"
fi

echo "Copying image to switch..."
"${SSHPASS_BASE[@]}" scp "${SSH_OPTS[@]}" "$LOCAL_IMG" "${USER}@${SCP_HOST}:/home/${USER}" || {
  echo "SCP failed"
  exit 1
}

sleep 1

echo "Listing remote directory (sanity check)..."
"${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" "ls -l \"$REMOTE_PATH\"" || {
  echo "Remote file not found after copy"
  exit 1
}

sleep 1

echo "Fetching CPLD firmware into NVOS..."
"${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" \
  "nv action fetch platform firmware CPLD1 file://$REMOTE_PATH" || {
  echo "nv fetch failed"
  exit 1
}

sleep 1

echo "Installing CPLD firmware..."
"${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" \
  "nv action install platform firmware CPLD1 files \"$REMOTE_IMG\"" || {
  echo "nv install failed"
  exit 1
}

sleep 1

echo "Deleting fetched CPLD firmware file from NVOS..."
"${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" \
  "nv action delete platform firmware CPLD1 files \"$REMOTE_IMG\"" || {
  echo "nv delete failed"
  exit 1
}

echo "Done."
