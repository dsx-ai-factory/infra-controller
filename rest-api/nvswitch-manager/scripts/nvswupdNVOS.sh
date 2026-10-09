#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -uo pipefail

SWITCH_IP="${1:-}"
USER="${2:-}"
PASS="${3:-}"
LOCAL_NVos="${4:-}"

if [[ -z "$SWITCH_IP" || -z "$USER" || -z "$PASS" || -z "$LOCAL_NVos" ]]; then
  echo "Usage: $0 <SWITCH_IP> <USER> <PASSWORD> <LOCAL_NVos_FILE>"
  exit 1
fi

if [[ ! -f "$LOCAL_NVos" ]]; then
  echo "NVOS file '$LOCAL_NVos' not found"
  exit 1
fi

NVOS_File="$(basename "$LOCAL_NVos")"
REMOTE_PATH="/home/${USER}/${NVOS_File}"

SSHPASS_BASE=(sshpass -p "$PASS")
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)

# SCP needs brackets around IPv6 hosts; SSH and ping use the original address.
SCP_HOST="$SWITCH_IP"
if [[ "$SCP_HOST" == *:* && "$SCP_HOST" != \[*\] ]]; then
  SCP_HOST="[$SCP_HOST]"
fi

echo "Copying NVOS file to switch..."
"${SSHPASS_BASE[@]}" scp "${SSH_OPTS[@]}" "$LOCAL_NVos" "${USER}@${SCP_HOST}:/home/${USER}" || {
  echo "SCP failed"
  exit 1
}
sleep 1

echo "Listing remote NVOS file..."
"${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" "ls -l \"$REMOTE_PATH\"" || {
  echo "Remote file not found after copy"
  exit 1
}
sleep 1

echo "Fetching NVOS system image into NVOS..."
"${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" \
  "nv action fetch system image file://$REMOTE_PATH" || {
  echo "nv fetch failed"
  exit 1
}
sleep 1

echo "Installing NVOS system image (force)..."
INSTALL_OUTPUT=$("${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" \
  "nv action install system image files \"$NVOS_File\" force" 2>&1) || {
  echo "nv install failed"
  echo "$INSTALL_OUTPUT"
  exit 1
}
echo "$INSTALL_OUTPUT"
sleep 1

# Check if a new image was actually installed (vs already installed)
isNewImageInstalled=true
if echo "$INSTALL_OUTPUT" | grep -q "already installed. Reboot skipped"; then
  echo "Image already installed, skipping reboot wait..."
  isNewImageInstalled=false
fi

############################################
# Wait for switch to go down, then come back (only if new image installed)
############################################

if $isNewImageInstalled; then
  echo -n "Waiting for ${SWITCH_IP} to go down"
  while ping -W 1 -c 1 "$SWITCH_IP" &>/dev/null; do
    echo -n "."
    sleep 2
  done
  echo

  echo -n "Waiting for ${SWITCH_IP} to come back up"
  while ! ping -W 1 -c 1 "$SWITCH_IP" &>/dev/null; do
    echo -n "."
    sleep 2
  done
  echo

  echo "Switch is back online, waiting a bit for services to start..."
  sleep 60
fi

############################################
# Always show current firmware version
############################################

echo "Current running system version..."
"${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" \
  "nv show system version" || {
  echo "nv show version failed"
  exit 1
}

############################################
# Always try to uninstall old system image
############################################

echo "Uninstalling old system image..."
if ! "${SSHPASS_BASE[@]}" ssh "${SSH_OPTS[@]}" "${USER}@${SWITCH_IP}" \
  "nv action uninstall system image force"; then
  if $isNewImageInstalled; then
    echo "nv uninstall failed"
    exit 1
  else
    echo "nv uninstall failed (expected - no old image to remove)"
  fi
fi

echo "NVOS update script finished."
