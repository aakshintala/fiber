# Google Generative AI probe

Ticket: #136. Platform: macOS, 2026-09-29, Gemini API (`generativelanguage.googleapis.com/v1beta`), direct.

## Method

`probe.py`, `probe2.py`, `probe3.py` (run in that order; logs `run.log`, `run2.log`, `run3.log`). Raw status, headers, request and body are in `raw/`; the key is redacted, including in URLs.

Prices (https://ai.google.dev/gemini-api/docs/pricing, standard paid tier, per million tokens): gemini-3.1-flash-lite $0.25 in / $1.50 out; gemini-2.5-flash-lite $0.10 / $0.40. Estimated spend on Gemini: under $0.01 across about 100 requests (cap $1.00).

The 2.x side is not reachable. `gemini-2.5-flash-lite`, `gemini-2.5-flash` and `gemini-2.5-pro` all answer HTTP 404 "no longer available to new users" for this key (`raw/lite-404-gemini-2.5-flash-lite.json`; the same for the others in `raw/*gemini-2.5-flash.json`). The one 2.x model that answered is `gemini-2.5-flash-image`, which rejects function calling ("Function calling is not enabled for this model", `raw/x2-flash-image-tool.json`). So every 2.x half of a row below is unmeasured, except where noted. All other results are `gemini-3.1-flash-lite` (the 3.x model the brief named for the comparison rows; it also stood in for the single-model rows because 2.5-flash-lite is closed).

## Results

API key (header vs `?key=`). On 3.1-flash-lite both a header and `?key=` return 200 with the same reply (`raw/auth2-header-*.json`, `raw/auth2-query-*.json`), and `?key=` also works on `streamGenerateContent?alt=sse` (`raw/auth2-query-stream.txt`). A bad header with a good `?key=` returns 200 (`raw/auth2-badheader-goodquery.json`). On `gemini-2.5-flash` (before its model check), a bad header alone returned 400 "API key not valid" and no key returned 403 (`raw/auth-badheader.json`, `raw/auth-none.json`). Both work on gemini-2.5-flash-image too (`raw/auth3-*`). Verdict: both work identically.

`systemInstruction.role`. `user`, `model` and no role all return 200 and the model answers PINEAPPLE as instructed (`raw/sysrole-{user,model,None}-gemini-3.1-flash-lite.json`). On gemini-2.5-flash-image all three are accepted (200) but none is obeyed (reply "Hello."; `raw/sysrole2-*`). That model is an image model, so this is weak evidence about 2.x text models.

Tool schemas. On 3.1: `parametersJsonSchema` with `$defs`/`$ref`/`anyOf`/`additionalProperties` returns 200 with a correct call (`raw/schema-json-schema-in-parametersJsonSchema-gemini-3.1-flash-lite.json`); the same schema in `parameters` returns 400 `Unknown name "$ref"` (`raw/schema-json-schema-in-parameters-*`); the OpenAPI subset (inline object, no `$ref`) in `parameters` returns 200. The 400 for `$ref` in `parameters` also came back from `gemini-2.5-flash` (`raw/schema-json-schema-in-parameters-gemini-2.5-flash.json`), before the model check, so it is a request-level rule. Whether 2.x accepts `parametersJsonSchema` is unmeasured (404).

Function-call ids. 3.1 accepts an `id` on `functionCall`/`functionResponse` and answers normally for `call_abc-123`, `call abc/123!` (outside the pi pattern), a 65-character id, and a mismatched response id (`raw/id2-*`). In the `functionCall`s sampled (about 20 in `raw/val*` and `raw/id2-emitted-*`) 3.1 put an `id` on each, for example `call_603161` (`raw/id2-emitted-gemini-3.1-flash-lite.json`). Separate finding: a replayed `functionCall` part without `thoughtSignature` returns 400 "missing a thought_signature" (`raw/thought-signature-missing.json` shows the 400; the requests in `raw/id-*-gemini-3.1-flash-lite.json` carry the documented dummy `skip_thought_signature_validator`, which lets them through). Whether 2.x rejects an id is unmeasured. Every 2.x row is unmeasured because the 2.x text models return 404 for this key.

Images in `functionResponse.parts`. 3.1 accepts a 16x16 PNG there and names its colour: red for a red PNG (`raw/imgparts-gemini-3.1-flash-lite.json`), blue for a blue one (`raw/imgparts2-blue-gemini-3.1-flash-lite.json`). 2.x is unmeasured (404; gemini-2.5-flash-image has no function calling).

SSE `error` object. Not reached. Tried, on `streamGenerateContent?alt=sse`: `maxOutputTokens` 10,000,000 (200 normal stream on 3.1), a 3,000-token generation on 3.1 (`raw/sse2-long-gen.json`, ended with `finishReason: MAX_TOKENS`), an invalid `responseMimeType`, empty `contents`, and a `functionResponse` with no matching call (the invalid mime type and empty contents returned an ordinary HTTP 400 JSON body, not an SSE event: `raw/sse-bad-mime.json`, `raw/sse-empty-contents.json`; the `functionResponse` request returned 404, model unavailable: `raw/sse-toolresponse-noname.json`). No request produced a `data:` line containing an `error` object. A mid-stream error needs a server fault, which we cannot trigger cheaply.

`VALIDATED`. 3.1 accepts `AUTO`, `ANY`, `NONE` and `VALIDATED` (200 each; `raw/mode-*`); the lowercase spelling `validated` is also accepted (`raw/mode-lowercase-validated.json`). A prompt asking for `unit: "kelvin"` (schema enum `c`/`f`) and `n: 3.7` (integer): `AUTO` returned the violating arguments in 6 of 6 samples; `VALIDATED` returned schema-valid arguments in 6 of 6 (`unit` `c` or `f`, `n` 3) (`raw/val2-*`). With a plain "hi" prompt `VALIDATED` returns text, so it does not force a call (`raw/val-allowed-names.json`). Whether 2.x accepts `VALIDATED` is unmeasured (404).

## Malformed replies

None from a model. Two harness notes: the first-pass 3.1 history requests failed with a `thoughtSignature` 400 until the dummy signature was added; that 400 is a documented requirement, not a model fault.

## For the owner

- Safety finish reasons, made-up ids, explicit caching: not probed. On made-up ids: I sent four ids on replay (`call_abc-123`, `call abc/123!`, a 65-character id, and a `functionResponse` id different from the call's), each with the dummy `thoughtSignature`; each returned 200 and a normal answer. In the samples, the model's own emitted ids looked like `call_603161`.
- Design choices the results bear on (not ruled): the key can go in the header or the query; `systemInstruction.role` can be omitted; `parametersJsonSchema` takes the schema tested and `parameters` rejects `$ref`.
- This key got 404 for the 2.5 text models (`gemini-2.5-flash-lite`, `gemini-2.5-flash`, `gemini-2.5-pro`). `gemini-2.5-flash-image` answered but has no function calling.
