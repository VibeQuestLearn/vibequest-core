import assert from "node:assert/strict";

const passed = {
  valid: true,
  rules: [{ ruleId: "reviewed-boundary", status: "passed" }],
};

const failed = (ruleId) => ({
  valid: false,
  rules: [{ ruleId, status: "failed" }],
});

export const hiddenCases = [
  {
    caseId: "ua-non-unified-shielded",
    run(api) {
      assert.equal(
        api.requireShieldedRecipient(
          failed("zip316.receiver.policy"),
          { network: "testnet", preferredSupportedReceiver: "sapling" },
          "testnet",
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "zip321-transparent-memo",
    run(api) {
      assert.equal(
        api.acceptPaymentRequest(
          failed("zip321.memo.policy"),
          { paymentCount: 1, totalZatoshis: 100_000_000, memoCount: 1 },
          { paymentCount: 1, amountZatoshis: 100_000_000 },
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "viewing-malformed",
    run(api) {
      assert.equal(
        api.acceptViewingAuthority(
          failed("zip316.viewing-key.parse"),
          { authority: "incoming", canSpend: false },
        ).ok,
        false,
      );
    },
  },
  {
    caseId: "lifecycle-mismatch",
    run(api) {
      assert.equal(
        api.nextPaymentState(
          {
            observationId: "obs_mismatch",
            observationCount: 1,
            currentConfirmations: 10,
            requiredConfirmations: 10,
            currentHeight: 3_000_010,
            expiresAtHeight: 3_000_040,
            previouslyConfirmed: false,
            present: true,
            recipientMatches: false,
            amountMatches: true,
          },
          new Set(),
        ),
        "mismatched",
      );
    },
  },
  {
    caseId: "privacy-google-wallet-linkage",
    run(api) {
      const event = api.privacySafeEvent({
        requestId: "req_hidden",
        scenarioId: "shielded-checkout",
        state: "confirmed",
        ruleIds: ["payment.lifecycle.state"],
        userId: "usr_hidden",
        googleEmail: "hidden@example.test",
        rawAddress: "hidden-address",
        memo: "hidden-memo",
        paymentRequest: "zcash:hidden",
        viewingKey: "uview-hidden",
      });
      assert.deepEqual(Object.keys(event).sort(), [
        "requestId",
        "ruleIds",
        "scenarioId",
        "state",
      ]);
      const encoded = JSON.stringify(event);
      assert.equal(encoded.includes("hidden@example.test"), false);
      assert.equal(encoded.includes("hidden-address"), false);
      assert.equal(encoded.includes("hidden-memo"), false);
    },
  },
];
