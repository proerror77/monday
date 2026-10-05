#!/bin/sh
set -eu
# Only a non-root Pod init container runs this. Mounted Secret projection may
# use links; install owned regular files in private tmpfs for Rust/PostgreSQL.
umask 077
mkdir -p /private/tls
for name in ca.crt server.crt server.key; do
  test -f "/projection/$name"
  cp "/projection/$name" "/private/tls/$name"
  chmod 600 "/private/tls/$name"
done

if test -d /runtime; then
  # Create subPath directories as the service UID before Kubernetes mounts them.
  mkdir -p /runtime/logs /runtime/tmp /runtime/run
  chmod 700 /runtime/logs /runtime/tmp /runtime/run
fi

if test -d /credentials; then
  mkdir -p /private/credentials
  for name in kube-ca.crt kube-token artifact-reader-token artifact-reader.pem; do
    test -f "/credentials/$name"
    cp "/credentials/$name" "/private/credentials/$name"
    chmod 600 "/private/credentials/$name"
  done
fi
