#!/usr/bin/env bash
# Build dist/mcdx_ql-<version>-bundle.tar.gz (crate + docs + JAR).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name=="mcdx_ql") | .version')"
echo "version=${VERSION}"

cargo package --no-verify --allow-dirty
cargo doc --no-deps --document-private-items
./scripts/build-jar.sh

DEST="dist/mcdx_ql-${VERSION}"
rm -rf "${DEST}"
mkdir -p "${DEST}/docs" "${DEST}/java"

cp "target/package/mcdx_ql-${VERSION}.crate" "${DEST}/"
cp -a target/doc/. "${DEST}/docs/"
cp "java/target/mcdx-ql-${VERSION}.jar" "${DEST}/java/"
# Also keep a copy of the native lib next to the JAR for explicit System.loadLibrary setups
mkdir -p "${DEST}/java/native"
cp -a java/src/main/resources/native/. "${DEST}/java/native/" 2>/dev/null || true

cat > "${DEST}/USAGE.txt" <<EOF
mcdx_ql ${VERSION}

Contents:
  mcdx_ql-${VERSION}.crate     — Rust cargo package
  docs/                        — rustdoc HTML (docs/mcdx_ql/index.html)
  java/mcdx-ql-${VERSION}.jar  — Java bindings (JNI; native lib embedded)
  java/native/                 — raw native libs (optional)

Rust (sibling private repos):
  mcdx_ql = { git = "https://github.com/nilpferdschaefer/mcdx-ql", tag = "v${VERSION}" }

Java:
  // Maven: install the JAR into a private repo, or path-depend in CI
  java -cp java/mcdx-ql-${VERSION}.jar com.nilpferdschaefer.mcdxql.SmokeTest

  String json = McdxQl.compile("{\\"expr\\":\\"AVG([close.1d], 14)\\",\\"assets\\":[\\"BTC\\"],\\"params\\":{\\"period\\":14}}");
EOF

mkdir -p dist
tar -C dist -czf "dist/mcdx_ql-${VERSION}-bundle.tar.gz" "mcdx_ql-${VERSION}"
ls -la dist/
echo "bundle=dist/mcdx_ql-${VERSION}-bundle.tar.gz"
