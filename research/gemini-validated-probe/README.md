# Gemini VALIDATED probe

Ticket: #1214. Platform: macOS, 2026-10-16, Gemini API (`generativelanguage.googleapis.com/v1beta`), direct.

## Method

`raw/probe.sh` (six requests, logs nothing but the HTTP status per request to stdout). Each request sends `ask_user`'s schema as `parametersJsonSchema` with `toolConfig.functionCallingConfig.mode` set to `VALIDATED`, and a prompt that invites one schema violation. Raw replies are in `raw/` (`A_baseline.json` through `F_empty_list.json`); no key is stored in any of them. All six requests returned HTTP 200.

## Results

Model: `gemini-3-flash-preview`. Constraints under test: `questions` `minItems: 1` / `maxItems: 4`, `header` `maxLength: 12`, `options` `minItems: 2` / `maxItems: 4`.

- A (baseline, one question with two options): one `ask_user` call, schema-valid.
- B (six questions requested): one `ask_user` call with four questions, each with two options. `maxItems: 4` on `questions` held in the sample.
- C (header Anglicised as one full sentence): one `ask_user` call whose `header` is "Which deployment environment should this service target today.", 62 characters against `maxLength: 12`. `maxLength` not enforced.
- D (exactly one option requested): one `ask_user` call with two options, the second label invented ("(Required)"). `minItems: 2` on `options` held in the sample.
- E (seven options requested): no call. The reply is text refusing, citing the four-option limit.
- F (empty questions list): one `ask_user` call with a single invented question ("Error"). `minItems: 1` on `questions` held in the sample.

## For the owner

- Verdict: `VALIDATED` on `gemini-3-flash-preview` does not enforce `maxLength`. The `minItems`/`maxItems` bounds held in these samples, but one sample each is weak evidence.
- Design choice the results bear on: Gemini requests keep `AUTO`, because `ask_user` and the session naming tool both declare `maxLength` (see `docs/model-routing.md`, "Google Generative AI wire facts").
