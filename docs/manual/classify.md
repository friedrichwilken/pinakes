# Classifying residue with a model

Reviewing every residue entry and near-duplicate pair by hand does not scale. `pinakes
classify` sends them to an OpenAI-compatible chat completions endpoint in batches and turns the
model's verdicts into ordinary `decisions.jsonl` lines, the same file `decide` and the
[curation skill](curating-with-an-agent.md) write to:

```sh
export PINAKES_LLM_URL=https://api.openai.com/v1     # or any OpenAI-compatible endpoint
export PINAKES_LLM_KEY=sk-...                        # optional; omitted when the endpoint needs none
export PINAKES_LLM_MODEL=gpt-4o-mini                 # or pass --model
pinakes classify --dry-run                           # print the proposed decisions, write nothing
pinakes classify --batch 20                          # write them to decisions.jsonl
```

`PINAKES_LLM_URL` is required — a missing endpoint is an error, never a silent skip. Candidates
are every undecided residue entry plus every near-duplicate pair's `duplicate` id from
`duplicates.jsonl` that has no active decision yet, sent in batches of `--batch` (default 20)
with id, title, excerpt, reason and context (a near-duplicate candidate's context names its
canonical id and similarity instead of a sidebar section). The model is asked for a JSON array
of `{"id", "decision": "include"|"exclude"|"unsure", "rationale", "confidence"}` at temperature
0, retried up to three times on a `429` or `5xx` response. Decisions are written with
`by: "classifier:<model>"`, so `report` and `residue list` show exactly who decided what.

Two rules hold regardless of what the model says: a residue page whose reason is `new_source` is
never written as `include` (a model proposing it is recorded as `unsure` instead, so a person
still looks at genuinely new sources before they enter the corpus), and `unsure` never enters
the manifest — `resolve` keeps treating an `unsure` page as residue, exactly as it does for a
human's `unsure` verdict from `decide`.
