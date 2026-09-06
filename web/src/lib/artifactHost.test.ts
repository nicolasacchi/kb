import { describe, expect, it } from "vitest";
import {
  artifactOrigin,
  deriveArtifactHostSuffix,
  encodeKbForHost,
  isArtifactOrigin,
  isOriginOfArtifact,
} from "./artifactHost";

// The unit env is `node` (see vitest.config.ts) — there is no `window`, so
// every call passes an explicit `loc`. That is also the honest shape: the
// artifact-host helpers are pure functions of (id, suffix, location).
function loc(href: string): Location {
  const u = new URL(href);
  return {
    protocol: u.protocol,
    hostname: u.hostname,
    port: u.port,
    origin: u.origin,
    href: u.href,
  } as Location;
}

// Invariant #7 — `artifact_host_suffix` is RUNTIME CONFIG. Every assertion
// below that matters is run against a NON-localhost suffix as well, so a
// hardcoded `.artifacts.localhost` anywhere in the implementation could not
// pass this file.
const PROD = loc("https://kb.example.com/");
const PROD_SUFFIX = ".artifacts.example.com";
const DEV = loc("http://localhost:4000/");
const DEV_SUFFIX = ".artifacts.localhost";

const A = "aaaaaaaaaaaa";
const B = "bbbbbbbbbbbb";
const KB = "canon";
const KB2 = "research";

describe("deriveArtifactHostSuffix", () => {
  it("drops the first label of a DNS parent", () => {
    expect(deriveArtifactHostSuffix(PROD)).toEqual({
      suffix: ".artifacts.example.com",
      portSuffix: "",
    });
  });
  it("keeps a bare hostname whole and preserves the port", () => {
    expect(deriveArtifactHostSuffix(DEV)).toEqual({
      suffix: ".artifacts.localhost",
      portSuffix: ":4000",
    });
  });
  it("keeps an IP literal whole (no DNS labels to strip)", () => {
    expect(deriveArtifactHostSuffix(loc("http://127.0.0.1:4000/")).suffix).toBe(
      ".artifacts.127.0.0.1",
    );
  });
});

describe("encodeKbForHost", () => {
  it("leaves a plain kb name untouched", () => {
    expect(encodeKbForHost("canon")).toBe("canon");
  });
  it("replaces every underscore with a dash", () => {
    expect(encodeKbForHost("obs_docs")).toBe("obs-docs");
    expect(encodeKbForHost("a_b_c")).toBe("a-b-c");
  });
});

describe("artifactOrigin (ARTIFACT HOST GRAMMAR v2 — qualified label)", () => {
  it("emits the qualified `<kb_enc>--<id>` label", () => {
    expect(artifactOrigin(A, KB, ".artifacts.example.test", PROD)).toBe(
      `https://${KB}--${A}.artifacts.example.test`,
    );
  });
  it("uses the daemon-supplied suffix over the heuristic", () => {
    expect(artifactOrigin(A, KB, ".artifacts.example.test", PROD)).toBe(
      `https://${KB}--${A}.artifacts.example.test`,
    );
  });
  it("preserves protocol + port from the parent", () => {
    expect(artifactOrigin(A, KB, DEV_SUFFIX, DEV)).toBe(
      `http://${KB}--${A}.artifacts.localhost:4000`,
    );
  });
  it("encodes '_' in the kb name to '-' in the host label", () => {
    expect(artifactOrigin(A, "obs_docs", PROD_SUFFIX, PROD)).toBe(
      `https://obs-docs--${A}.artifacts.example.com`,
    );
  });
});

describe("isArtifactOrigin (suffix-only trust boundary)", () => {
  it("accepts ANY artifact of the corpus — this is why it cannot attribute", () => {
    expect(
      isArtifactOrigin(artifactOrigin(A, KB, PROD_SUFFIX, PROD), PROD_SUFFIX, PROD),
    ).toBe(true);
    expect(
      isArtifactOrigin(artifactOrigin(B, KB2, PROD_SUFFIX, PROD), PROD_SUFFIX, PROD),
    ).toBe(true);
  });
  it("rejects a foreign origin", () => {
    expect(isArtifactOrigin("https://evil.example/", PROD_SUFFIX, PROD)).toBe(false);
  });
});

describe("isOriginOfArtifact (exact per-artifact attribution)", () => {
  it("accepts only the origin of THAT artifact — prod suffix", () => {
    const originA = artifactOrigin(A, KB, PROD_SUFFIX, PROD);
    expect(isOriginOfArtifact(originA, A, KB, PROD_SUFFIX, PROD)).toBe(true);
    // The bug this function exists to close: pane B's beacons must not be
    // accepted by pane A's handler (invariants #8 / #19).
    expect(isOriginOfArtifact(originA, B, KB, PROD_SUFFIX, PROD)).toBe(false);
  });

  it("accepts only the origin of THAT artifact — dev suffix + port", () => {
    const originA = artifactOrigin(A, KB, DEV_SUFFIX, DEV);
    expect(isOriginOfArtifact(originA, A, KB, DEV_SUFFIX, DEV)).toBe(true);
    expect(isOriginOfArtifact(originA, B, KB, DEV_SUFFIX, DEV)).toBe(false);
  });

  it("rejects the SAME id under a DIFFERENT kb — two corpora can mint the same 12-hex id", () => {
    const originA = artifactOrigin(A, KB, PROD_SUFFIX, PROD);
    expect(isOriginOfArtifact(originA, A, KB, PROD_SUFFIX, PROD)).toBe(true);
    expect(isOriginOfArtifact(originA, A, KB2, PROD_SUFFIX, PROD)).toBe(false);
  });

  it("is suffix-sensitive: the SAME id+kb under a different runtime suffix is rejected", () => {
    // #7 — a hardcoded suffix in the implementation would make this pass
    // when it must not.
    const originUnderOtherSuffix = artifactOrigin(A, KB, ".artifacts.other.test", PROD);
    expect(isOriginOfArtifact(originUnderOtherSuffix, A, KB, PROD_SUFFIX, PROD)).toBe(
      false,
    );
    expect(
      isOriginOfArtifact(originUnderOtherSuffix, A, KB, ".artifacts.other.test", PROD),
    ).toBe(true);
  });

  it("rejects a BARE (unqualified) origin against a qualified expectation", () => {
    // The pre-v2 shape (`<id>.artifacts.<root>`, no `<kb_enc>--` prefix) is
    // never the SAME string as the qualified origin, so a legacy/bare
    // artifact iframe can never be mistaken for the id it happens to share.
    expect(
      isOriginOfArtifact(`https://${A}.artifacts.example.com`, A, KB, PROD_SUFFIX, PROD),
    ).toBe(false);
  });

  it("rejects a prefix/suffix near-miss on the hostname", () => {
    // `<kb>--<A><B>.artifacts.example.com` ends with the right suffix and
    // CONTAINS the id, so a naive `includes`/`endsWith` check would accept
    // it.
    expect(
      isOriginOfArtifact(`https://${KB}--${A}${B}.artifacts.example.com`, A, KB, PROD_SUFFIX, PROD),
    ).toBe(false);
    expect(
      isOriginOfArtifact(`https://sub.${KB}--${A}.artifacts.example.com`, A, KB, PROD_SUFFIX, PROD),
    ).toBe(false);
  });

  it("rejects a protocol or port mismatch", () => {
    expect(
      isOriginOfArtifact(`http://${KB}--${A}.artifacts.example.com`, A, KB, PROD_SUFFIX, PROD),
    ).toBe(false);
    expect(
      isOriginOfArtifact(`http://${KB}--${A}.artifacts.localhost`, A, KB, DEV_SUFFIX, DEV),
    ).toBe(false);
  });

  it("rejects a foreign origin and empty inputs", () => {
    expect(isOriginOfArtifact("https://evil.example", A, KB, PROD_SUFFIX, PROD)).toBe(
      false,
    );
    expect(isOriginOfArtifact("null", A, KB, PROD_SUFFIX, PROD)).toBe(false);
    expect(isOriginOfArtifact("", A, KB, PROD_SUFFIX, PROD)).toBe(false);
    expect(
      isOriginOfArtifact(
        artifactOrigin(A, KB, PROD_SUFFIX, PROD),
        null,
        KB,
        PROD_SUFFIX,
        PROD,
      ),
    ).toBe(false);
    expect(
      isOriginOfArtifact(
        artifactOrigin(A, KB, PROD_SUFFIX, PROD),
        "",
        KB,
        PROD_SUFFIX,
        PROD,
      ),
    ).toBe(false);
    expect(
      isOriginOfArtifact(
        artifactOrigin(A, KB, PROD_SUFFIX, PROD),
        A,
        null,
        PROD_SUFFIX,
        PROD,
      ),
    ).toBe(false);
    expect(
      isOriginOfArtifact(
        artifactOrigin(A, KB, PROD_SUFFIX, PROD),
        A,
        "",
        PROD_SUFFIX,
        PROD,
      ),
    ).toBe(false);
  });

  it("is case-insensitive on the host (browsers lowercase serialized origins)", () => {
    expect(
      isOriginOfArtifact(
        `HTTPS://${KB.toUpperCase()}--${A.toUpperCase()}.ARTIFACTS.EXAMPLE.COM`,
        A,
        KB,
        PROD_SUFFIX,
        PROD,
      ),
    ).toBe(true);
  });
});
