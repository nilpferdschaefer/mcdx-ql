package com.nilpferdschaefer.mcdxql;

/** Smoke test for the packaged JAR + native library. */
public final class SmokeTest {
  public static void main(String[] args) {
    String req =
        "{"
            + "\"expr\":\"AVG([close.1d; $from:$to], $period)\","
            + "\"assets\":[\"BTC\",\"ETH\"],"
            + "\"params\":{\"period\":14,\"from\":1700000000000,\"to\":1700086400000},"
            + "\"after_ts\":-1,"
            + "\"limit\":16"
            + "}";
    String out = McdxQl.compile(req);
    if (!out.contains("\"ok\":true") || !out.contains("AVG(e.close)")) {
      System.err.println("smoke test failed: " + out);
      System.exit(1);
    }
    System.out.println("mcdx-ql java smoke ok");
  }
}
