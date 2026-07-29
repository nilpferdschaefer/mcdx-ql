# mcdx-ql binary repository

Versioned **Rust `.crate`** and **Java Maven** artifacts published by CI into this
[`binary`](https://github.com/nilpferdschaefer/mcdx-ql/tree/binary) branch.

Latest published from source: **0.1.0**

## Layout

| Path | Contents |
|------|----------|
| `crates/mcdx_ql-*.crate` | `cargo package` output |
| `maven/com/nilpferdschaefer/mcdx-ql/` | Maven2 repo (JAR + POM + metadata) |
| `bundles/mcdx_ql-*-bundle.tar.gz` | crate + rustdoc + JAR |

## Java (Maven / Gradle)

```xml
<repositories>
  <repository>
    <id>mcdx-ql-binary</id>
    <url>https://raw.githubusercontent.com/nilpferdschaefer/mcdx-ql/binary/maven</url>
  </repository>
</repositories>

<dependency>
  <groupId>com.nilpferdschaefer</groupId>
  <artifactId>mcdx-ql</artifactId>
  <version>0.1.0</version>
</dependency>
```

Gradle:

```kotlin
repositories {
    maven("https://raw.githubusercontent.com/nilpferdschaefer/mcdx-ql/binary/maven")
}
dependencies {
    implementation("com.nilpferdschaefer:mcdx-ql:0.1.0")
}
```

Private repos need a credential that can read this repository (same as any other
raw GitHub content fetch).

## Rust

Prefer a git dependency on a tag / `main`. To consume the packed crate from this
branch (offline / vendoring):

```bash
curl -fsSL -o mcdx_ql-0.1.0.crate \
  https://raw.githubusercontent.com/nilpferdschaefer/mcdx-ql/binary/crates/mcdx_ql-0.1.0.crate
# unpack / vendor as needed
```

See also `index.json` for the published file list.
