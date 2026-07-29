#!/usr/bin/env bash
# Populate a binary-repo tree with:
#   maven/   — Maven2 layout for the Java JAR (+ javadoc jar + pom + checksums)
#   crates/  — cargo package (.crate) + sha256
#   bundles/ — full dist bundle tarball
#   javadoc/ — Javadoc HTML site
#
# Dev mode: each publish *replaces* the previous tree (no version history retained).
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
JAVADOC_JAR_SRC="java/target/${ARTIFACT}-${VERSION}-javadoc.jar"
BUNDLE_SRC="dist/${CRATE_NAME}-${VERSION}-bundle.tar.gz"
POM_SRC="java/pom.xml"
JAVADOC_HTML_SRC="java/target/reports/apidocs"
if [[ ! -f "${JAVADOC_HTML_SRC}/index.html" && -f java/target/apidocs/index.html ]]; then
  JAVADOC_HTML_SRC="java/target/apidocs"
fi

for f in "${CRATE_SRC}" "${JAR_SRC}" "${JAVADOC_JAR_SRC}" "${BUNDLE_SRC}" "${POM_SRC}"; do
  if [[ ! -f "${f}" ]]; then
    echo "missing artifact: ${f} (run ./scripts/package-bundle.sh first)" >&2
    exit 1
  fi
done
if [[ ! -f "${JAVADOC_HTML_SRC}/index.html" ]]; then
  echo "missing javadoc HTML: ${JAVADOC_HTML_SRC}/index.html" >&2
  exit 1
fi

# Wipe prior published trees so we only keep the current overwrite.
rm -rf \
  "${DEST}/maven" \
  "${DEST}/crates" \
  "${DEST}/bundles" \
  "${DEST}/javadoc"
mkdir -p \
  "${DEST}/maven/${GROUP_PATH}/${ARTIFACT}/${VERSION}" \
  "${DEST}/crates" \
  "${DEST}/bundles" \
  "${DEST}/javadoc"

# --- crates ---
cp -f "${CRATE_SRC}" "${DEST}/crates/"
sha256sum "${CRATE_SRC}" | awk '{print $1}' > "${DEST}/crates/${CRATE_NAME}-${VERSION}.crate.sha256"

# --- bundles ---
cp -f "${BUNDLE_SRC}" "${DEST}/bundles/"
sha256sum "${BUNDLE_SRC}" | awk '{print $1}' > "${DEST}/bundles/${CRATE_NAME}-${VERSION}-bundle.tar.gz.sha256"

# --- javadoc HTML (overwrite in place) ---
cp -a "${JAVADOC_HTML_SRC}/." "${DEST}/javadoc/"

# --- maven artifact ---
MAVEN_DIR="${DEST}/maven/${GROUP_PATH}/${ARTIFACT}/${VERSION}"
cp -f "${JAR_SRC}" "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.jar"
cp -f "${JAVADOC_JAR_SRC}" "${MAVEN_DIR}/${ARTIFACT}-${VERSION}-javadoc.jar"
cp -f "${POM_SRC}" "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.pom"

checksums() {
  local f="$1"
  sha1sum "${f}" | awk '{print $1}' > "${f}.sha1"
  md5sum "${f}" | awk '{print $1}' > "${f}.md5"
  sha256sum "${f}" | awk '{print $1}' > "${f}.sha256"
}
checksums "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.jar"
checksums "${MAVEN_DIR}/${ARTIFACT}-${VERSION}-javadoc.jar"
checksums "${MAVEN_DIR}/${ARTIFACT}-${VERSION}.pom"

# --- maven-metadata.xml (current version only) ---
META="${DEST}/maven/${GROUP_PATH}/${ARTIFACT}/maven-metadata.xml"
UPDATED="$(date -u +%Y%m%d%H%M%S)"
{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<metadata>'
  echo '  <groupId>com.nilpferdschaefer</groupId>'
  echo "  <artifactId>${ARTIFACT}</artifactId>"
  echo '  <versioning>'
  echo "    <latest>${VERSION}</latest>"
  echo "    <release>${VERSION}</release>"
  echo '    <versions>'
  echo "      <version>${VERSION}</version>"
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
    if not p.name.endswith("-javadoc.jar")
)
javadoc_jars = sorted(
    str(p.relative_to(dest))
    for p in (dest / "maven").rglob("*-javadoc.jar")
)
bundles = sorted(p.name for p in (dest / "bundles").glob("*.tar.gz"))
index = {
    "package": "mcdx_ql",
    "mode": "dev-overwrite",
    "updated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "version": "${VERSION}",
    "crates": crates,
    "maven_jars": jars,
    "javadoc_jars": javadoc_jars,
    "javadoc_html": "javadoc/index.html",
    "bundles": bundles,
}
(dest / "index.json").write_text(json.dumps(index, indent=2) + "\n")
PY

cat > "${DEST}/README.md" <<EOF
# mcdx-ql binary repository

**Dev mode:** each CI publish overwrites this branch in place (no version history).

Current artifacts from source version **${VERSION}**.

## Layout

| Path | Contents |
|------|----------|
| \`crates/mcdx_ql-*.crate\` | \`cargo package\` output |
| \`maven/com/nilpferdschaefer/mcdx-ql/\` | Maven2 repo (JAR + javadoc JAR + POM) |
| \`javadoc/\` | Javadoc HTML |
| \`bundles/mcdx_ql-*-bundle.tar.gz\` | crate + rustdoc + JAR + javadoc |

## Javadoc

- Branch: [\`javadoc/\`](https://github.com/nilpferdschaefer/mcdx-ql/tree/binary/javadoc)
- Pages: https://nilpferdschaefer.github.io/mcdx-ql/javadoc/
- Maven classifier: \`mcdx-ql-${VERSION}-javadoc.jar\`

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

## Rust

Prefer a git dependency on \`main\`. Packed crate (overwritten each publish):

\`\`\`bash
curl -fsSL -o mcdx_ql-${VERSION}.crate \\
  https://raw.githubusercontent.com/nilpferdschaefer/mcdx-ql/binary/crates/mcdx_ql-${VERSION}.crate
\`\`\`

See also \`index.json\`.
EOF

echo "published ${VERSION} → ${DEST} (dev overwrite)"
ls -la "${DEST}/crates/" "${DEST}/bundles/" "${MAVEN_DIR}/"
test -f "${DEST}/javadoc/index.html"
