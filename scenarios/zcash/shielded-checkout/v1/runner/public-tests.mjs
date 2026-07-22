import assert from "node:assert/strict";

const passed = {
  valid: true,
  rules: [{ ruleId: "reviewed-boundary", status: "passed" }],
};

const failed = (ruleId) => ({
  valid: false,
  rules: [{ ruleId, status: "failed" }],
});

const confirmed = {
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

const diagnostic = {
  requestId: "req_01",
  scenarioId: "shielded-checkout",
  state: "confirmed",
  ruleIds: ["payment.lifecycle.state"],
  userId: "usr_01",
  googleEmail: "learner@example.test",
  rawAddress: "test-address",
  memo: "test-memo",
  paymentRequest: "zcash:test-only",
  viewingKey: "uview-test-only",
};

export const publicCases = [
  {
    caseId: "ua-valid-shielded",
    run(api) {
      const result = api.requireShieldedRecipient(
        passed,
        { network: "testnet", preferredSupportedReceiver: "orchard" },
        "testnet",
      );
      assert.equal(result.ok, true);
      assert.equal(result.value, "orchard");
    },
  },
  {
    caseId: "ua-wrong-network",
    run(api) {
      assert.equal(
        api.requireShieldedRecipient(
          passed,
          { network: "mainnet", preferredSupportedReceiver: "orchard" },
          "testnet",
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "ua-transparent-only",
    run(api) {
      assert.equal(
        api.requireShieldedRecipient(
          passed,
          { network: "testnet", preferredSupportedReceiver: "transparent" },
          "testnet",
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "zip321-valid-exact",
    run(api) {
      assert.equal(
        api.acceptPaymentRequest(
          passed,
          { paymentCount: 1, totalZatoshis: 100_000_000, memoCount: 1 },
          { paymentCount: 1, amountZatoshis: 100_000_000 },
        ).ok,
        true,
      );
    },
  },
  {
    caseId: "zip321-unknown-required",
    run(api) {
      assert.equal(
        api.acceptPaymentRequest(
          failed("zip321.parameters.required"),
          { paymentCount: 1, totalZatoshis: 100_000_000, memoCount: 0 },
          { paymentCount: 1, amountZatoshis: 100_000_000 },
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "zip321-lab-amount-limit",
    run(api) {
      assert.equal(
        api.acceptPaymentRequest(
          passed,
          { paymentCount: 1, totalZatoshis: 200_000_000, memoCount: 0 },
          { paymentCount: 1, amountZatoshis: 100_000_000 },
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "viewing-valid-incoming",
    run(api) {
      const result = api.acceptViewingAuthority(passed, {
        authority: "incoming",
        canSpend: false,
      });
      assert.equal(result.ok, true);
      assert.equal(result.value, "incoming");
    },
  },
  {
    caseId: "viewing-spending-material",
    run(api) {
      assert.equal(
        api.acceptViewingAuthority(passed, {
          authority: "full",
          canSpend: true,
        }).ok,
        false,
      );
    },
  },
  {
    caseId: "viewing-wrong-network",
    run(api) {
      assert.equal(
        api.acceptViewingAuthority(
          failed("zip316.viewing-key.network"),
          { authority: "incoming", canSpend: false },
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "lifecycle-confirmed",
    run(api) {
      assert.equal(api.nextPaymentState(confirmed, new Set()), "confirmed");
    },
  },
  {
    caseId: "lifecycle-reorg",
    run(api) {
      assert.equal(
        api.nextPaymentState(
          {
            ...confirmed,
            present: false,
            currentConfirmations: 0,
            previouslyConfirmed: true,
          },
          new Set(),
        ),
        "reorged",
      );
    },
  },
  {
    caseId: "lifecycle-duplicate",
    run(api) {
      assert.equal(
        api.nextPaymentState(confirmed, new Set(["obs_01"])),
        "duplicated",
      );
    },
  },
  {
    caseId: "privacy-safe-event",
    run(api) {
      assert.deepEqual(Object.keys(api.privacySafeEvent(diagnostic)).sort(), [
        "requestId",
        "ruleIds",
        "scenarioId",
        "state",
      ]);
    },
  },
  {
    caseId: "privacy-raw-address-log",
    run(api) {
      assert.equal(
        JSON.stringify(api.privacySafeEvent(diagnostic)).includes("test-address"),
        false,
      );
    },
  },
  {
    caseId: "privacy-memo-log",
    run(api) {
      assert.equal(
        JSON.stringify(api.privacySafeEvent(diagnostic)).includes("test-memo"),
        false,
      );
    },
  },
];
