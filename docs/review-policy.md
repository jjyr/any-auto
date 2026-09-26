# Shared review policy

Pi, agy, Jev, OpenAI and the agentapi transport share a typed classification contract
and one local decision function. Backend transports cannot return an execution
decision directly.

```text
Pi extension / agy hook
          |
Local bypasses / blacklist / circuit breaker
          |
ReviewInput: action + user window + evidence + completeness
          |
          +----------------------------------------------------------+
          |                           |                              |
     prompts/prompt.txt          prompts/prompt.txt          prompts/questions.json
     Pi reviewer                 agy reviewer                    Jev API
     JSON classification         JSON classification             Choices + probabilities
          |                           |                              |
          +------------- Parse / validate / normalize ---------------+
                                      |
                                Typed Judgment
                  risk / authorization / policy / rationale
                           optional probabilities
                                      |
                           Shared policy decision
                     completeness + classification rules
                        + optional probability gates
                                      |
                                 Assessment
                            allow / deny + diagnostics
                                      |
                           Audit / breaker / hook result
```

Backend inputs live in `src/prompts/`: `prompt.txt` supplies the default
conversational prompt; `questions.json` supplies Jev Choice questions.
Pi/agy do not read or append the questions file. Their default prompt preserves
the original risk/authorization definitions and allowed/denied actions, with
the output adapted to the typed classification contract.
`approver.instructions.risk`, `.authorization`, and `.policy` customize Jev only.
Provider switches reset inherited prompts and instructions; `context_budget_bytes`
and `probability_threshold` remain inherited.

Conversational models must return JSON such as:

```json
{
  "risk": "high",
  "authorization": "medium",
  "policy": "permitted",
  "rationale": "The user authorized this narrowly scoped operation."
}
```

Risk: `low`, `medium`, `high`, `critical`, `unknown`.
Authorization: `high`, `medium`, `low`, `unknown`.
Policy: `permitted`, `needs_confirmation`, `prohibited`, `unknown`.

`Judgment` deserializes these fields into Rust enums. All four fields are required,
rationale must be nonblank, and missing required fields or invalid enum values fail closed.
Additional fields are ignored, including extra probability keys and model-produced
outcome/decision fields; they never override the local decision. JSON fenced in prose
is accepted. An old `{"outcome":"allow"}` response still fails because it lacks
the required classifications. Policy compliance includes exact
targets, scope, user restrictions and absolute prohibitions, not just risk.

## Local outcome rules

- Transport/schema failures and incomplete required evidence deny.
- Critical risk or prohibited policy deny.
- Unknown risk or unknown/needs_confirmation policy deny.
- Permitted low/medium risk passes without a separate authorization gate.
- Permitted high risk requires medium/high authorization. The model's permitted
  classification must establish narrow scope and compliance with concrete rules.
- An optional probability distribution adds threshold checks. Without it, no
  probability is fabricated and no probability gate is applied.

Internally, classifications, outcomes, rule IDs and check kinds use enums.
Probability distributions use fixed-field structs; approval checks use typed
structures. Jev responses deserialize into typed answers and directly construct
the same Judgment. JSON is used at transport and diagnostic output boundaries.

A distribution, when present, must cover all known options in all three dimensions.
Additional fields are ignored.
Values must be finite, within [0, 1], and agree with the selected maximum.
Approximate sums are preserved, following the official TypeSafe SDK.

With probabilities, the routine route requires P(low)+P(medium) to meet the
threshold. Otherwise the authorized route accepts selected low/medium/high with
P(low)+P(medium)+P(high) meeting the threshold and selected medium/high authorization
with P(high)+P(medium) meeting it. Both require selected permitted policy and
P(permitted) meeting the threshold. This preserves the existing Jev semantics.

Jev must supply distributions; a malformed or missing distribution is an error.
The default Pi/agy prompt does not request probabilities. `probability_threshold` defaults
to 0.9 and is used only when distributions exist. `confidence` is not a substitute.

## Migration and boundaries

The former Pi/agy model-produced outcome is no longer authoritative. Update custom
review prompts and mock responses to the new contract. A top-level `prompt` or
`ANY_AUTO_PROMPT` replaces the entire conversational prompt. Custom prompts must request the typed
JSON contract above. Jev rejects this conversational setting; use its
`approver.instructions` instead.

All reviewed providers now enforce required-evidence completeness locally.
Read-only bypasses, command blacklists, circuit-breaker behavior and the configurable
user-message window remain at the existing common boundary. Reviewers still
cannot invoke tools. A `needs_confirmation` classification yields deny; only
the outer circuit breaker denies further reviewed actions until a new validated user message.

Policy/rubric versions are `policy-decision-v6` and `review-v6`. Session fingerprints
include the policy version and effective prompt so old reviewer sessions are not
silently reused with the new output contract. Audit metadata includes the normalized
judgment, rule ID, checks and optional probabilities.

The `reviewer-eval` runner supports Jev, Pi and agy CLI and invokes the same local
decision function as production. `--concurrency` defaults to 1; each trial uses
an isolated fresh session even when running concurrently. Shared rules do not guarantee that different models will classify the
same evidence identically. See [evaluation usage](../evals/README.md).
