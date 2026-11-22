# Sync as a service: cost model and pilot plan

Status: a draft for [#46](https://github.com/thestephenromano/matters/issues/46), for the maintainer's decisions.
- No pricing is chosen and no outreach has happened.
- No real user data enters a pilot until the release gate of #45 is met.
- Each figure below is either measured, with its host and workload, or marked as an assumption.

## Cost model

Monthly cost per active collection is:

```text
  storage    = retained bytes x storage price
  + transfer = (uploads + downloads + catch-up) x transfer price
  + compute  = request rate x CPU per request (relay) + PostgreSQL share
  + support  = incidents per collection x time per incident x hourly cost
  + recovery = restores per collection x time per restore x hourly cost
```

| Term | Value | Status |
| --- | --- | --- |
| Retained bytes | edits x 4.8 KB + checkpoints (about 4.7% of history each) | measured on Matters' workloads (`docs/evidence/sync-checkpoint-cost-2026-11-21.json`). Edits per collection per month: **assumption, to measure in the pilot** |
| History budget | unlimited in this version: nothing is pruned | a policy decision. Pruning needs its own approval (SYNC_CHECKPOINTS.md) |
| Offline-device budget | any age: history is kept whole | follows from the history budget |
| Transfer | each record sent once and received by each other device, plus catch-up after restores | per-route counts from relay metrics: **to measure** |
| Compute | relay request rate and latency from `matters_relay_requests_total` and `request_duration_seconds` | **to measure**. Catch-up is currently quadratic in records (#241) and is the largest compute risk |
| Support and recovery | per incident | **to record in the pilot** |

The prices themselves, for storage, transfer, compute and staff time, are inputs the maintainer supplies for the deployment chosen. Self-hosting needs none of them (SELF_HOSTING.md).

## Candidate packaging (hypotheses)

These are hypotheses to test, not offers:
- **Self-hosted:** free, with the operator's own infrastructure. Support is best effort.
- **Hosted:** priced per active collection, with explicit limits on retained bytes and devices. No promise of unlimited history or of availability beyond what the deployment's failure domain supports (ADR 0003, SELF_HOSTING.md).

## Design-partner plan

1. **Consent first.** Before anyone is contacted, the maintainer records:
   - who will be approached, and why;
   - what they are asked;
   - what data, if any, they would trust to the pilot. The data stays synthetic until #45's gate is met.
2. **Two or three developers** who would integrate sync into their own application, through SYNC_INTEGRATION.md.
3. **Record for each partner:**
   - integration effort, and the questions they raised;
   - the operational expectations they state (availability, support, data location);
   - willingness to pay. Hypotheses ("would consider") are kept apart from commitments ("will pay X for Y").
4. **Decision.** Go or no-go for a separate service, with thresholds the maintainer sets in advance:
   - a number of committed partners;
   - integration effort within a bound;
   - cost per collection within a bound of the price they accept.

   A no-go keeps sync as a Matters feature.
