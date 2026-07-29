#!/usr/bin/env bash
# Build libmcdx_ql (JNI) and package the Java JAR with the native lib embedded.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name=="mcdx_ql") | .version')"
PROFILE="${PROFILE:-release}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

echo "==> cargo build --features jni (${PROFILE})"
if [[ "${PROFILE}" == "release" ]]; then
  cargo build --release --features jni
  LIB_DIR="${TARGET_DIR}/release"
else
  cargo build --features jni
  LIB_DIR="${TARGET_DIR}/debug"
fi

OS="$(uname -s)"
ARCH="$(uname -m)"
case "${OS}" in
  Linux) OS_KEY=linux; LIB_NAME=libmcdx_ql.so ;;
  Darwin) OS_KEY=macos; LIB_NAME=libmcdx_ql.dylib ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) OS_KEY=windows; LIB_NAME=mcdx_ql.dll ;;
  *) echo "unsupported OS: ${OS}" >&2; exit 1 ;;
esac
case "${ARCH}" in
  x86_64|amd64) ARCH_KEY=x86_64 ;;
  aarch64|arm64) ARCH_KEY=aarch64 ;;
  *) echo "unsupported arch: ${ARCH}" >&2; exit 1 ;;
esac

NATIVE_RES="java/src/main/resources/native/${OS_KEY}-${ARCH_KEY}"
mkdir -p "${NATIVE_RES}"
cp -f "${LIB_DIR}/${LIB_NAME}" "${NATIVE_RES}/"
echo "==> staged ${NATIVE_RES}/${LIB_NAME}"

# Keep pom version in sync with Cargo.toml
python3 - <<PY
from pathlib import Path
import re
version = "${VERSION}"
p = Path("java/pom.xml")
text = p.read_text()
text2, n = re.subn(
    r"(<artifactId>mcdx-ql</artifactId>\s*<version>)[^<]+(</version>)",
    rf"\g<1>{version}\g<2>",
    text,
    count=1,
)
if n != 1:
    raise SystemExit(f"failed to patch pom version (n={n})")
p.write_text(text2)
PY

if command -v mvn >/dev/null 2>&1; then
  echo "==> mvn package"
  (cd java && mvn -q -DskipTests package)
else
  echo "==> mvn not found; compiling JAR with javac"
  JAVA_OUT="java/target/classes"
  mkdir -p "${JAVA_OUT}"
  find java/src/main/java -name '*.java' > /tmp/mcdx_ql_sources.txt
  javac --release 21 -d "${JAVA_OUT}" @"/tmp/mcdx_ql_sources.txt"
  mkdir -p java/target
  JAR_PATH="java/target/mcdx-ql-${VERSION}.jar"
  jar cf "${JAR_PATH}" -C "${JAVA_OUT}" .
  jar uf "${JAR_PATH}" -C java/src/main/resources .
  echo "built ${JAR_PATH}"
fi

JAR="$(ls -1 java/target/mcdx-ql-${VERSION}.jar 2>/dev/null | head -1)"
if [[ -z "${JAR}" ]]; then
  JAR="$(ls -1 java/target/mcdx-ql-*.jar | head -1)"
fi
echo "==> JAR: ${JAR}"

# Smoke test
echo "==> smoke test"
java -cp "${JAR}" com.nilpferdschaefer.mcdxql.SmokeTest

echo "OK mcdx-ql-${VERSION} java bindings"
