#!/usr/bin/env bash
# Populate a binary-repo tree with:
#   maven/  — Maven2 layout for the Java JAR (+ pom + checksums + metadata)
#   crates/ — cargo package (.crate) + sha256
#   bundles/— full dist bundle tarball
#
# Usage:
#   ./scripts/package-bundle.sh          # build artifacts first
#   ./scripts/publish-binary-repo.sh DEST
#
# DEST is typically a checkout of the `binary` branch.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DEST="${1:-}"
if [[ -z "${DEST}" ]]; then
  echo "usage: $0 <dest-dir>" >&2
  exit 1
fi

VERSION="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name=="mcdx_ql") | .version')"
GROUP_PATH="com/nilpferdschaefer"
ARTIFACT="mcdx-ql"
CRATE_NAME="mcdx_ql"

CRATE_SRC="target/package/${CRATE_NAME}-${VERSION}.crate"
JAR_SRC="java/target/${ARTIFACT}-${VERSION}.jar"
BUNDLE_SRC="dist/${CRATE_NAME}-${VERSION}-bundle.tar.gz"
POM_SRC="java/pom.xml"

for f in "${CRATE_SRC}" "${JAR_SRC}" "${BUNDLE_SRC}" "${POM_SRC}"; do
  if [[ ! -f "${f}" ]]; then
    echo "missing artifact: ${f} (run ./scripts/package-bundle.sh first)" >&2
    exit 1
  fi
done

mkdir -p \
  "${DEST}/maven/${GROUP_PATH}/${ARTIFACT}/${VERSION}" \
  "${DEST}/crates" \
  "${DEST}/bundles"

# --- crates ---
cp -f "${CRATE_SRC}" "${DEST}/crates/"
sha256sum "${CRATE_SRC}" | awk '{print $1}' > "${DEST}/crates/${CRATE_NAME}-${VERSION}.crate.sha256"

# --- bundles ---
cp -f "${BUNDLE_SRC}" "${DEST}/bundles/"
sha256sum "${BUNDLE_SRC}" | awk '{print $1}' > "${DEST}/bundles/${CRATE_NAME}-${VERSION}-bundle.tar.gz.sha256"

# --- maven artifact ---
MAVEN_DIR="${DEST}/maven/${GROUP_PATH}/${ARTIFACT}/${VERSION}"
cp -f "${JAR_SRC}" "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.jar"
cp -f "${POM_SRC}" "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.pom"

checksums() {
  local f="$1"
  sha1sum "${f}" | awk '{print $1}' > "${f}.sha1"
  md5sum "${f}" | awk '{print $1}' > "${f}.md5"
  sha256sum "${f}" | awk '{print $1}' > "${f}.sha256"
}
checksums "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.jar"
checksums "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.pom"

# --- maven-metadata.xml (merge versions) ---
META="${DEST}/maven/${GROUP_PATH}/${ARTIFACT}/maven-metadata.xml"
EXISTING_VERSIONS=()
if [[ -f "${META}" ]]; then
  while IFS= read -r v; do
    [[ -n "${v}" ]] && EXISTING_VERSIONS+=("${v}")
  done < <(python3 - <<'PY' "${META}"
import sys, re
text = open(sys.argv[1]).read()
print("\n".join(re.findall(r"<version>([^<]+)</version>", text)))
PY
)
fi

# unique, sorted (version-ish lexical; fine for semver X.Y.Z)
ALL_VERSIONS=()
seen=()
for v in "${EXISTING_VERSIONS[@]}" "${VERSION}"; do
  skip=0
  for s in "${seen[@]+"${seen[@]}"}"; do
    if [[ "${s}" == "${v}" ]]; then skip=1; break; fi
  done
  if [[ "${skip}" -eq 0 ]]; then
    seen+=("${v}")
    ALL_VERSIONS+=("${v}")
  fi
done
IFS=$'\n' ALL_VERSIONS=($(printf '%s\n' "${ALL_VERSIONS[@]}" | sort -V))
unset IFS
LATEST="${ALL_VERSIONS[-1]}"
UPDATED="$(date -u +%Y%m%d%H%M%S)"

{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<metadata>'
  echo '  <groupId>com.nilpferdschaefer</groupId>'
  echo "  <artifactId>${ARTIFACT}</artifactId>"
  echo '  <versioning>'
  echo "    <latest>${LATEST}</latest>"
  echo "    <release>${LATEST}</release>"
  echo '    <versions>'
  for v in "${ALL_VERSIONS[@]}"; do
    echo "      <version>${v}</version>"
  done
  echo '    </versions>'
  echo "    <lastUpdated>${UPDATED}</lastUpdated>"
  echo '  </versioning>'
  echo '</metadata>'
} > "${META}"
checksums "${META}"

# --- index + README ---
python3 - <<PY
import json, pathlib
from datetime import datetime, timezone
dest = pathlib.Path("${DEST}")
crates = sorted(p.name for p in (dest / "crates").glob("*.crate"))
jars = sorted(
    str(p.relative_to(dest))
    for p in (dest / "maven").rglob("*.jar")
)
bundles = sorted(p.name for p in (dest / "bundles").glob("*.tar.gz"))
index = {
    "package": "mcdx_ql",
    "updated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "latest": "${VERSION}",
    "crates": crates,
    "maven_jars": jars,
    "bundles": bundles,
}
(dest / "index.json").write_text(json.dumps(index, indent=2) + "\n")
PY

cat > "${DEST}/README.md" <<EOF
# mcdx-ql binary repository

Versioned **Rust \`.crate\`** and **Java Maven** artifacts published by CI into this
[\`binary\`](https://github.com/nilpferdschaefer/mcdx-ql/tree/binary) branch.

Latest published from source: **${VERSION}**

## Layout

| Path | Contents |
|------|----------|
| \`crates/mcdx_ql-*.crate\` | \`cargo package\` output |
| \`maven/com/nilpferdschaefer/mcdx-ql/\` | Maven2 repo (JAR + POM + metadata) |
| \`bundles/mcdx_ql-*-bundle.tar.gz\` | crate + rustdoc + JAR |

## Java (Maven / Gradle)

\`\`\`xml
<repositories>
  <repository>
    <id>mcdx-ql-binary</id>
    <url>https://raw.githubusercontent.com/nilpferdschaefer/mcdx-ql/binary/maven</url>
  </repository>
</repositories>

<dependency>
  <groupId>com.nilpferdschaefer</groupId>
  <artifactId>mcdx-ql</artifactId>
  <version>${VERSION}</version>
</dependency>
\`\`\`

Gradle:

\`\`\`kotlin
repositories {
    maven("https://raw.githubusercontent.com/nilpferdschaefer/mcdx-ql/binary/maven")
}
dependencies {
    implementation("com.nilpferdschaefer:mcdx-ql:${VERSION}")
}
\`\`\`

Private repos need a credential that can read this repository (same as any other
raw GitHub content fetch).

## Rust

Prefer a git dependency on a tag / \`main\`. To consume the packed crate from this
branch (offline / vendoring):

\`\`\`bash
curl -fsSL -o mcdx_ql-${VERSION}.crate \\
  https://raw.githubusercontent.com/nilpferdschaefer/mcdx-ql/binary/crates/mcdx_ql-${VERSION}.crate
# unpack / vendor as needed
\`\`\`

See also \`index.json\` for the published file list.
EOF

echo "published ${VERSION} → ${DEST}"
ls -la "${DEST}/crates/" "${DEST}/bundles/" "${MAVEN_DIR}/"
