package com.nilpferdschaefer.mcdxql;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Objects;

/**
 * Java façade over the {@code mcdx_ql} Rust compiler (JNI).
 *
 * <p>Request/response are JSON (see crate {@code compile_json} / README).
 *
 * <p>Single indicator:
 *
 * <pre>{@code
 * String response = McdxQl.compile(
 *     "{"
 *         + "\"expr\":\"AVG([close.1d; $from:$to], $period)\","
 *         + "\"assets\":[\"BTC\",\"ETH\"],"
 *         + "\"params\":{\"period\":14,\"from\":1700000000000,\"to\":1700086400000}"
 *         + "}");
 * }</pre>
 *
 * <p>Multi-indicator batch with a shared emit range (close / vol / beta / ema). The
 * batch postfix {@code [$from:$to]} is inherited by every series — equivalent to
 * writing {@code ; $from:$to} on each {@code [close.1h; …]}:
 *
 * <pre>{@code
 * String panel = McdxQl.compile(
 *     "{"
 *         + "\"expr\":"
 *         + "\"{ "
 *         + "close: [close.1h@self], "
 *         + "vol: STD(RET([close.1h@self]), $vol_n) * SQRT($bars_per_year), "
 *         + "beta: REGR(RET([close.1h@self]), RET([close.1h@$benchmark]), $beta_n), "
 *         + "ema: EMA([close.1h@self], $ema_n) "
 *         + "}[$from:$to]\","
 *         + "\"assets\":[\"ETH\",\"SOL\",\"AVAX\"],"
 *         + "\"params\":{"
 *         + "\"from\":1700000000000,"
 *         + "\"to\":1700086400000,"
 *         + "\"vol_n\":14,"
 *         + "\"beta_n\":31,"
 *         + "\"ema_n\":14,"
 *         + "\"bars_per_year\":8760,"
 *         + "\"benchmark\":\"BTC\""
 *         + "}"
 *         + "}");
 * }</pre>
 *
 * <p>{@code @self} is required on every series once any member references another
 * asset (here {@code $benchmark} in {@code beta}).
 */
public final class McdxQl {
  static {
    NativeLoader.load();
  }

  private McdxQl() {}

  /**
   * Compile an indicator expression request.
   *
   * <p>The {@code expr} field may be a single expression (e.g. {@code REGR(…)}) or a
   * named batch. When every series shares one absolute emit range, attach it once as
   * a batch postfix instead of repeating {@code ; $from:$to} on each series:
   *
   * <pre>{@code
   * {
   *   close: [close.1h@self],
   *   vol:   STD(RET([close.1h@self]), $vol_n) * SQRT($bars_per_year),
   *   beta:  REGR(RET([close.1h@self]), RET([close.1h@$benchmark]), $beta_n),
   *   ema:   EMA([close.1h@self], $ema_n)
   * }[$from:$to]
   * }</pre>
   *
   * @param requestJson JSON object with {@code expr}, {@code assets}, {@code params}, etc.
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
