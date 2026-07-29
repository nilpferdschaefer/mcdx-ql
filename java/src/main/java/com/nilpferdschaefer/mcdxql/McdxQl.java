package com.nilpferdschaefer.mcdxql;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Objects;

/**
 * Java façade over the {@code mcdx_ql} Rust compiler (JNI).
 *
 * <p>Request/response are JSON (see crate {@code compile_json} / README). Example:
 *
 * <pre>{@code
 * String response = McdxQl.compile("""
 *   {
 *     "expr": "AVG([close.1d; $from:$to], $period)",
 *     "assets": ["BTC", "ETH"],
 *     "params": {"period": 14, "from": 1700000000000, "to": 1700086400000}
 *   }
 *   """);
 * }</pre>
 */
public final class McdxQl {
  static {
    NativeLoader.load();
  }

  private McdxQl() {}

  /**
   * Compile an indicator expression request.
   *
   * @param requestJson JSON object with {@code expr}, {@code assets}, {@code params}, …
   * @return JSON envelope {@code {"ok":true,...}} or {@code {"ok":false,"error":{...}}}
   */
  public static String compile(String requestJson) {
    Objects.requireNonNull(requestJson, "requestJson");
    return compileNative(requestJson);
  }

  /**
   * Compile from a UTF-8 JSON file.
   *
   * @param path path to request JSON
   * @return JSON response envelope
   */
  public static String compileFile(Path path) throws java.io.IOException {
    byte[] bytes = Files.readAllBytes(path);
    return compile(new String(bytes, StandardCharsets.UTF_8));
  }

  /** JNI entrypoint implemented in Rust ({@code jni_bridge}). */
  private static native String compileNative(String requestJson);
}
