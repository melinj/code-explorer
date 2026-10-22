/** Default for an outcome callback the caller does not need. */
function ignore() {
  // Nothing to do: the caller only wants the value `chargeOnce` resolves with.
}

/**
 * Charges a payment method once and reports the outcome through callbacks, so the payment
 * dialog can stay a plain view. A declined card is an expected outcome, not an error: it
 * resolves to null after `onDeclined` has run.
 *
 * @param {object}   args
 * @param {number}   args.amountCents
 * @param {string}   args.methodId
 * @param {Function} args.charge       - (methodId, cents) => Promise<receipt>
 * @param {Function} [args.onPaid]     - (receipt) => void
 * @param {Function} [args.onDeclined] - (reason: string) => void
 */
export async function chargeOnce({
  amountCents,
  methodId,
  charge,
  onPaid = ignore,
  onDeclined = ignore,
}) {
  if (!(amountCents > 0)) {
    onDeclined('The amount must be more than zero.');
    return null;
  }
  try {
    const receipt = await charge(methodId, amountCents);
    onPaid(receipt);
    return receipt;
  } catch (err) {
    onDeclined(err instanceof Error ? err.message : String(err));
    return null;
  }
}
