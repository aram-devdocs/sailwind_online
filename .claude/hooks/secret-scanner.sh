#!/usr/bin/env bash
# PreToolUse (Write|Edit|MultiEdit): block writing obvious secrets into the tree.
# This repo is public, so a committed private key or live credential is a leak.
# Scans the whole payload (covers Write content, Edit new_string, and MultiEdit
# edits in one pass) plus the target path. Exit 2 to block, 0 otherwise.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

hook_read_payload
path="$(hook_field '.tool_input.file_path')"

# Private keys of any flavour (RSA, EC, OpenSSH, DSA, PGP, or bare).
if printf '%s' "$HOOK_PAYLOAD" |
  grep -Eq -- '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----'; then
  hook_block "Blocked: a private key block would be written into a public repo."
fi

# AWS access key id (AKIA/ASIA + 16 uppercase-alnum).
if printf '%s' "$HOOK_PAYLOAD" | grep -Eq -- '\b(AKIA|ASIA)[0-9A-Z]{16}\b'; then
  hook_block "Blocked: an AWS access key id would be written into a public repo."
fi

# AWS secret access key assignment (40-char base64-ish value).
if printf '%s' "$HOOK_PAYLOAD" |
  grep -Eiq -- 'aws_secret_access_key[[:space:]]*[=:][[:space:]]*[A-Za-z0-9/+]{40}'; then
  hook_block "Blocked: an AWS secret access key would be written into a public repo."
fi

# A .env file carrying a credential-shaped assignment with a real value.
case "$path" in
*.env | *.env.* | *.env)
  if printf '%s' "$HOOK_PAYLOAD" |
    grep -Eiq -- '(password|secret|token|api[_-]?key|access[_-]?key|private[_-]?key)[[:space:]]*=[[:space:]]*[^"[:space:]]{8,}'; then
    hook_block "Blocked: a .env file with credential-shaped values must not enter the repo."
  fi
  ;;
esac

exit 0
