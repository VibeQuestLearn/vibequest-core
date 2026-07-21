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

export function requireShieldedRecipient(
  report: RuleReport,
  summary: AddressSummary,
  expectedNetwork: Network,
): Decision<ShieldedReceiver> {
  if (!report.valid || summary.network !== expectedNetwork) return { ok: false, reason: "recipient-policy" };
  if (
    summary.preferredSupportedReceiver !== "orchard" &&
    summary.preferredSupportedReceiver !== "sapling"
  ) {
    return { ok: false, reason: "shielded-receiver-required" };
  }
  return { ok: true, value: summary.preferredSupportedReceiver };
}

export function acceptPaymentRequest(
  report: RuleReport,
  summary: PaymentSummary,
  expected: CheckoutIntent,
): Decision<PaymentSummary> {
  const exactAmount = summary.totalZatoshis === expected.amountZatoshis;
  const exactPaymentCount = summary.paymentCount === expected.paymentCount;
  if (!report.valid || !exactAmount || !exactPaymentCount) {
    return { ok: false, reason: "payment-intent-mismatch" };
  }
  return { ok: true, value: summary };
}

export function acceptViewingAuthority(
  report: RuleReport,
  summary: ViewingSummary,
): Decision<ViewingSummary["authority"]> {
  if (!report.valid || summary.canSpend) return { ok: false, reason: "viewing-key-boundary" };
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

export function nextPaymentState(
  observation: PaymentObservation,
  seenObservationIds: ReadonlySet<string>,
): PaymentState {
  if (observation.present && (!observation.recipientMatches || !observation.amountMatches)) {
    return "mismatched";
  }
  if (observation.observationId && seenObservationIds.has(observation.observationId)) return "duplicated";
  if (observation.observationCount > 1) return "duplicated";
  if (observation.previouslyConfirmed && !observation.present) return "reorged";
  if (
    observation.present &&
    observation.currentConfirmations >= observation.requiredConfirmations
  ) {
    return "confirmed";
  }
  if (!observation.present && observation.currentHeight > observation.expiresAtHeight) {
    return "expired";
  }
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

export function privacySafeEvent(
  event: CheckoutDiagnosticInput,
): PrivacySafeEvent {
  return { requestId: event.requestId, scenarioId: event.scenarioId, state: event.state, ruleIds: event.ruleIds };
}
