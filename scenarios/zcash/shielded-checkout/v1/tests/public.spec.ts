import {
  acceptPaymentRequest,
  acceptViewingAuthority,
  nextPaymentState,
  privacySafeEvent,
  requireShieldedRecipient,
  type PaymentObservation,
  type RuleReport,
} from "../starter/src/checkout";

const passed: RuleReport = {
  valid: true,
  rules: [{ ruleId: "reviewed-boundary", status: "passed" }],
};

describe("step 01: Unified Address receiver policy", () => {
  test("ua-valid-shielded selects Orchard", () => {
    expect(
      requireShieldedRecipient(
        passed,
        { network: "testnet", preferredSupportedReceiver: "orchard" },
        "testnet",
      ),
    ).toEqual({ ok: true, value: "orchard" });
  });

  test("ua-wrong-network is denied", () => {
    expect(
      requireShieldedRecipient(
        passed,
        { network: "mainnet", preferredSupportedReceiver: "orchard" },
        "testnet",
      ),
    ).toEqual({ ok: false, reason: "recipient-policy" });
  });

  test("ua-transparent-only is denied", () => {
    expect(
      requireShieldedRecipient(
        passed,
        { network: "testnet", preferredSupportedReceiver: "transparent" },
        "testnet",
      ).ok,
    ).toBe(false);
  });
});

describe("step 02: ZIP-321 request intent", () => {
  test("zip321-valid-exact is accepted", () => {
    expect(
      acceptPaymentRequest(
        passed,
        { paymentCount: 1, totalZatoshis: 100_000_000, memoCount: 1 },
        { paymentCount: 1, amountZatoshis: 100_000_000 },
      ).ok,
    ).toBe(true);
  });

  test("zip321-unknown-required report is denied", () => {
    expect(
      acceptPaymentRequest(
        { valid: false, rules: [{ ruleId: "zip321.parameters.required", status: "failed" }] },
        { paymentCount: 1, totalZatoshis: 100_000_000, memoCount: 0 },
        { paymentCount: 1, amountZatoshis: 100_000_000 },
      ).ok,
    ).toBe(false);
  });

  test("zip321-lab-amount-limit cannot satisfy a smaller intent", () => {
    expect(
      acceptPaymentRequest(
        passed,
        { paymentCount: 1, totalZatoshis: 200_000_000, memoCount: 0 },
        { paymentCount: 1, amountZatoshis: 100_000_000 },
      ).ok,
    ).toBe(false);
  });
});

describe("step 03: view-only authority", () => {
  test("viewing-valid-incoming is accepted", () => {
    expect(
      acceptViewingAuthority(passed, { authority: "incoming", canSpend: false }),
    ).toEqual({ ok: true, value: "incoming" });
  });

  test("viewing-spending-material is denied", () => {
    expect(
      acceptViewingAuthority(passed, { authority: "full", canSpend: true }).ok,
    ).toBe(false);
  });

  test("viewing-wrong-network report is denied", () => {
    expect(
      acceptViewingAuthority(
        { valid: false, rules: [{ ruleId: "zip316.viewing-key.network", status: "failed" }] },
        { authority: "incoming", canSpend: false },
      ).ok,
    ).toBe(false);
  });
});

describe("step 04: deterministic payment lifecycle", () => {
  const confirmed: PaymentObservation = {
    observationId: "obs_01",
    observationCount: 1,
    currentConfirmations: 10,
    requiredConfirmations: 10,
    currentHeight: 3_000_010,
    expiresAtHeight: 3_000_040,
    previouslyConfirmed: false,
    present: true,
    recipientMatches: true,
    amountMatches: true,
  };

  test("lifecycle-confirmed fulfills at the threshold", () => {
    expect(nextPaymentState(confirmed, new Set())).toBe("confirmed");
  });

  test("lifecycle-reorg withdraws prior confirmation", () => {
    expect(
      nextPaymentState(
        { ...confirmed, present: false, currentConfirmations: 0, previouslyConfirmed: true },
        new Set(),
      ),
    ).toBe("reorged");
  });

  test("lifecycle-duplicate denies replay", () => {
    expect(nextPaymentState(confirmed, new Set(["obs_01"]))).toBe("duplicated");
  });
});

describe("step 05: privacy-safe diagnostics", () => {
  const diagnostic = {
    requestId: "req_01",
    scenarioId: "shielded-checkout",
    state: "confirmed" as const,
    ruleIds: ["payment.lifecycle.state"],
    userId: "usr_01",
    googleEmail: "learner@example.test",
    rawAddress: "raw-address",
    memo: "private memo",
    paymentRequest: "zcash:sensitive",
    viewingKey: "uview-sensitive",
  };

  test("privacy-safe-event keeps only allowlisted evidence", () => {
    expect(Object.keys(privacySafeEvent(diagnostic)).sort()).toEqual([
      "requestId",
      "ruleIds",
      "scenarioId",
      "state",
    ]);
  });

  test("privacy-raw-address-log is denied by omission", () => {
    expect(JSON.stringify(privacySafeEvent(diagnostic))).not.toContain("raw-address");
  });

  test("privacy-memo-log is denied by omission", () => {
    expect(JSON.stringify(privacySafeEvent(diagnostic))).not.toContain("private memo");
  });
});
