package com.nilpferdschaefer.mcdxql;

/**
 * Loads {@code libmcdx_ql} from a JAR resource or {@code java.library.path}.
 *
 * <p>Resource layout: {@code /native/<os>-<arch>/libmcdx_ql.so|.dylib|.dll}
 */
final class NativeLoader {
  private static final String LIB = "mcdx_ql";
  private static volatile boolean loaded;

  private NativeLoader() {}

  static synchronized void load() {
    if (loaded) {
      return;
    }
    try {
      System.loadLibrary(LIB);
      loaded = true;
      return;
    } catch (UnsatisfiedLinkError ignored) {
      // fall through to classpath resource
    }

    String resource = resourcePath();
    try (var in = NativeLoader.class.getResourceAsStream(resource)) {
      if (in == null) {
        throw new UnsatisfiedLinkError(
            "native library not found on java.library.path and missing classpath resource "
                + resource);
      }
      var dir = java.nio.file.Files.createTempDirectory("mcdx_ql_native_");
      dir.toFile().deleteOnExit();
      var fileName = resource.substring(resource.lastIndexOf('/') + 1);
      var target = dir.resolve(fileName);
      java.nio.file.Files.copy(in, target);
      target.toFile().deleteOnExit();
      System.load(target.toAbsolutePath().toString());
      loaded = true;
    } catch (UnsatisfiedLinkError e) {
      throw e;
    } catch (Exception e) {
      throw new UnsatisfiedLinkError("failed to load " + resource + ": " + e.getMessage());
    }
  }

  static String resourcePath() {
    String os = osKey();
    String arch = archKey();
    String file = libFileName();
    return "/native/" + os + "-" + arch + "/" + file;
  }

  private static String osKey() {
    String os = System.getProperty("os.name", "").toLowerCase(java.util.Locale.ROOT);
    if (os.contains("linux")) return "linux";
    if (os.contains("mac") || os.contains("darwin")) return "macos";
    if (os.contains("win")) return "windows";
    throw new UnsatisfiedLinkError("unsupported os.name=" + os);
  }

  private static String archKey() {
    String arch = System.getProperty("os.arch", "").toLowerCase(java.util.Locale.ROOT);
    if (arch.equals("amd64") || arch.equals("x86_64")) return "x86_64";
    if (arch.equals("aarch64") || arch.equals("arm64")) return "aarch64";
    throw new UnsatisfiedLinkError("unsupported os.arch=" + arch);
  }

  private static String libFileName() {
    return switch (osKey()) {
      case "linux" -> "libmcdx_ql.so";
      case "macos" -> "libmcdx_ql.dylib";
      case "windows" -> "mcdx_ql.dll";
      default -> throw new UnsatisfiedLinkError("unsupported os");
    };
  }
}
