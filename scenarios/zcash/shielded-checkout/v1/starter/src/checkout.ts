export type Network = "mainnet" | "testnet";
export type ShieldedReceiver = "orchard" | "sapling";

export type RuleReport = {
  valid: boolean;
  rules: Array<{ ruleId: string; status: "passed" | "failed" }>;
};

export type AddressSummary = {
  network: Network;
  preferredSupportedReceiver: "orchard" | "sapling" | "transparent" | null;
};

export type PaymentSummary = {
  paymentCount: number;
  totalZatoshis: number | null;
  memoCount: number;
};

export type CheckoutIntent = {
  amountZatoshis: number;
  paymentCount: number;
};

export type ViewingSummary = {
  authority: "incoming" | "full";
  canSpend: boolean;
};

export type Decision<T> =
  | { ok: true; value: T }
  | { ok: false; reason: string };

// VQ_DEFECT:defect-address-policy
export function requireShieldedRecipient(
  report: RuleReport,
  summary: AddressSummary,
  _expectedNetwork: Network,
): Decision<ShieldedReceiver> {
  if (!report.valid || summary.preferredSupportedReceiver === null) {
    return { ok: false, reason: "recipient-policy" };
  }
  return {
    ok: true,
    value: summary.preferredSupportedReceiver as ShieldedReceiver,
  };
}

// VQ_DEFECT:defect-request-intent
export function acceptPaymentRequest(
  report: RuleReport,
  summary: PaymentSummary,
  expected: CheckoutIntent,
): Decision<PaymentSummary> {
  if (
    !report.valid ||
    summary.totalZatoshis === null ||
    summary.totalZatoshis < expected.amountZatoshis
  ) {
    return { ok: false, reason: "payment-intent-mismatch" };
  }
  return { ok: true, value: summary };
}

// VQ_DEFECT:defect-viewing-authority
export function acceptViewingAuthority(
  report: RuleReport,
  summary: ViewingSummary,
): Decision<ViewingSummary["authority"]> {
  if (!report.valid) return { ok: false, reason: "invalid-key" };
  return { ok: true, value: summary.authority };
}

export type PaymentState =
  | "pending"
  | "confirmed"
  | "reorged"
  | "duplicated"
  | "expired"
  | "mismatched";

export type PaymentObservation = {
  observationId: string | null;
  observationCount: number;
  currentConfirmations: number;
  requiredConfirmations: number;
  currentHeight: number;
  expiresAtHeight: number;
  previouslyConfirmed: boolean;
  present: boolean;
  recipientMatches: boolean;
  amountMatches: boolean;
};

// VQ_DEFECT:defect-lifecycle-replay
export function nextPaymentState(
  observation: PaymentObservation,
  _seenObservationIds: ReadonlySet<string>,
): PaymentState {
  if (observation.present) return "confirmed";
  if (observation.currentHeight > observation.expiresAtHeight) return "expired";
  return "pending";
}

export type CheckoutDiagnosticInput = {
  requestId: string;
  scenarioId: string;
  state: PaymentState;
  ruleIds: string[];
  userId?: string;
  googleEmail?: string;
  rawAddress?: string;
  memo?: string;
  paymentRequest?: string;
  viewingKey?: string;
};

export type PrivacySafeEvent = {
  requestId: string;
  scenarioId: string;
  state: PaymentState;
  ruleIds: string[];
};

// VQ_DEFECT:defect-privacy-event
export function privacySafeEvent(
  event: CheckoutDiagnosticInput,
): PrivacySafeEvent {
  return { ...event };
}
