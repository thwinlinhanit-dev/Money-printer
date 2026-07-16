# unknown-order (P1)

An order is in the UNKNOWN state — we sent it but never confirmed its fate.

## Symptoms
- UNKNOWN-order alert with the intent/order id.

## Diagnosis
- Query the venue by client-order-id for its true status (filled/rejected/live).
- UNKNOWN means the account may have exposure we are not tracking.

## Remediation
- Resolve to a known state from venue truth before any new intent for that
  symbol. `/kill <venue>` to stop new orders while resolving.
- Never assume it rejected — assume it may have filled until proven otherwise.

## Escalation
Venue can't confirm ⇒ `/flatten` and page the owner. Unknown exposure is the
single most dangerous state (spec 007 UNKNOWN handling).
