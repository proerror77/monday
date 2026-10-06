#!/bin/sh
set -eu
# Preserve all existing identity journals. Create only the private parent.
umask 077
mkdir -p /state/attempt-identities
chmod 700 /state/attempt-identities
# The broker owner must have prepared its private projection. Do not chmod,
# replace or reset another writer's capabilities and existing tokens.
test -d /broker/private
test -f /broker/private/artifacts.json
