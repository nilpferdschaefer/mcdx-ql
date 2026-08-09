#!/usr/bin/env bash
# Build JAR + crate and deploy to GitHub Packages (Maven).
# Used by ci.yml (main → SNAPSHOT) and release.yml (tag → immutable version).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BASE="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name=="mcdx_ql") | .version')"

if [[ "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
  TAG="${GITHUB_REF_NAME}"
  EXPECTED="v${BASE}"
  if [[ "${TAG}" != "${EXPECTED}" ]]; then
    echo "Tag ${TAG} does not match Cargo.toml version ${EXPECTED}" >&2
    exit 1
  fi
  PUBLISH_VERSION="${BASE}"
  MODE="release"
else
  PUBLISH_VERSION="${BASE}-SNAPSHOT"
  MODE="snapshot"
fi

CRATE="${ROOT}/target/package/mcdx_ql-${BASE}.crate"
REPO_URL="https://maven.pkg.github.com/${GITHUB_REPOSITORY}"

echo "Publishing com.nilpferdschaefer:mcdx-ql:${PUBLISH_VERSION} (${MODE})"

chmod +x scripts/*.sh
./scripts/build-jar.sh
cargo package --no-verify --allow-dirty
test -f "${CRATE}"

python3 - <<PY
from pathlib import Path
import re
version = "${PUBLISH_VERSION}"
repo = "${REPO_URL}"
p = Path("java/pom.xml")
text = p.read_text()
text, n = re.subn(
    r"(<artifactId>mcdx-ql</artifactId>\s*<version>)[^<]+(</version>)",
    rf"\g<1>{version}\g<2>",
    text,
    count=1,
)
if n != 1:
    raise SystemExit(f"failed to stamp pom version (n={n})")
text, n = re.subn(
    r"(<github.maven.repo>)[^<]+(</github.maven.repo>)",
    rf"\g<1>{repo}\g<2>",
    text,
    count=1,
)
if n != 1:
    raise SystemExit(f"failed to stamp github.maven.repo (n={n})")
p.write_text(text)
print(f"pom version={version} repo={repo}")
PY

(
  cd java
  mvn -B -DskipTests deploy \
    -s "${GITHUB_WORKSPACE}/settings.xml" \
    "-Dcrate.file=${CRATE}"
)

{
  echo "## GitHub Packages (Maven)"
  echo ""
  echo "- Package: \`com.nilpferdschaefer:mcdx-ql:${PUBLISH_VERSION}\`"
  echo "- Mode: \`${MODE}\`"
  echo "- Registry: \`${REPO_URL}\`"
} >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
