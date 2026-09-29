# Images: which crate resizes them, and to what limits (issue #130)

Question: which crate decodes and resizes images before a provider module sends
them, and what limits does each protocol get?

All runs are dated 2026-09-29. Rust is 1.98.1.

## Method

- Crate measurement: two features in `research/dependency-rss`.
  - `image` is the `image` crate 0.25.10 with `default-features = false` and
    only `png`, `jpeg`, `gif`, `webp`.
  - `image-parts` is the smallest set of separate crates found that decodes the
    four formats, resizes, and re-encodes: zune-jpeg 0.5.15, png 0.18.1, gif
    0.14.2, image-webp 0.2.4, fast_image_resize 6.1.0, jpeg-encoder 0.7.1. The
    other resizers on crates.io (`resize` 0.8.9, `pic-scale` 0.7.12) were not
    measured. `image` itself uses the same zune-jpeg, png, gif and image-webp
    crates, so a separate set can only differ in the resizer and the glue.
  - Workload: decode a 4000x3000 JPEG (quality 90, 3.6 MiB) and a 4000x3000 PNG
    (screenshot-like, 237 KiB); fit inside 2000x2000 keeping the aspect ratio
    (Lanczos3, never enlarging); re-encode the JPEG at quality 80 and the PNG as
    PNG; do each file twice and assert the two outputs are byte-identical. It
    also decodes a GIF and a WebP. Fixtures come from `gen/` (deterministic, not
    committed: `cd gen && cargo run --release -- ../fixtures`).
  - Figures are the median of 5 runs. Linux is peak RSS and macOS is peak memory
    footprint, both over an empty program. Linux ran on GitHub's
    `ubuntu-24.04` (AMD EPYC 7763) and `ubuntu-24.04-arm` runners through
    `linux-probe/probe.yml`, on a throwaway branch `probe/130-image` that is
    deleted. Run 36545862616. macOS is Apple M3 Pro.
- Limits: each vendor's own documentation, then one matrix of live requests per
  vendor with `live.py` (base64 in the request body, prompt "state the width and
  height"). Cases: `flat-8000x6000` (a 950 KB PNG, flat colour, cheap),
  `flat-9000x9000` (1.2 MB PNG), `small-gif` (a 400x300 GIF), `noise-2400`
  (2400x2400 random-byte PNG, 17.3 MB, 23.0 MB as base64, cheap because
  vendors resize before billing).

Prices and spend (input / output per million tokens, from each vendor's pricing
page, looked up 2026-09-29):

| Model | Price | Page | Spend |
|---|---|---|---:|
| claude-sonnet-5-5 | $2 / $10 | https://platform.claude.com/docs/en/about-claude/pricing | about $0.011 |
| gpt-6-luna | $0.10 / $0.50 (short context) | https://developers.openai.com/api/docs/pricing | about $0.002 (Responses and chat completions) |
| gemini-3.1-flash-lite | $0.25 / $1.50 | https://ai.google.dev/gemini-api/docs/pricing | about $0.001 |

The Gemini probe used `gemini-3.1-flash-lite`, not the `gemini-2.5-flash-lite`
the brief named: this key gets HTTP 404 "no longer available to new users" for
the 2.5 models. Spend is the sum of each run's `usage` at those prices.

## The crate

Measured (raw: `raw/linux/out-*/rss.txt`, `determinism_*.txt`, `tree_*.txt`;
macOS figures were run locally with the same commands and are in the table
below).

Peak over an empty program, all four files in sequence (peak is the largest
decode), KiB:

| Set | Linux x86_64 RSS | Linux arm64 RSS | macOS arm64 footprint | Crates | Stripped binary (x86_64 / arm64 / macOS) |
|---|---:|---:|---:|---:|---:|
| `image`, four codecs | 148,448 | 148,160 | 157,120 | 23 | 1,410 / 1,285 / 1,266 KiB |
| separate crates | 67,884 | 67,464 | 67,888 | 28 | 4,994 / 3,077 / 3,099 KiB |

The empty program is 323 KiB on Linux x86_64 and 331 KiB on macOS.
`image` is 1,087 KiB over baseline on Linux x86_64 (1,410 - 323), the separate
set 4,671 KiB.

By file (peak KiB, not over baseline; Linux x86_64 RSS): 4000x3000 JPEG
144,944 (`image`) and 69,876 (separate); 4000x3000 PNG 141,632 and 66,220;
GIF 4,128 and 4,876; WebP 4,872 and 5,324. An 81-megapixel PNG (9000x9000, a
1.2 MB file) peaks at 534,700 KiB in `image` and 307,988 KiB in the separate
set, on Linux x86_64 (macOS footprint: 533,460 and 305,660 KiB). Peak memory
follows pixel count, not file size, and a small file can be a decompression
bomb.

- Crates: `image` is 23 crates. The separate set is 28: the extra five are
  `fast_image_resize`'s `document-features` chain (`litrs`, `proc-macro2`,
  `quote`, `syn`, `unicode-ident`) and `thiserror`. Setting
  `default-features = false` with `no_std` on `fast_image_resize` did not reduce
  the count (29).
- C: none. `cargo tree` shows no `-sys` crate and no `cc` build dependency. The
  only build scripts are `crc32fast` and `num-traits`, which probe the
  compiler. All codecs are pure Rust. zune-jpeg uses `unsafe` for SIMD.
- Licences: every crate is MIT or Apache-2.0 or dual with one of them
  (`adler2` is `0BSD OR MIT OR Apache-2.0`; `moxcms` and `pxfm` are
  `BSD-3-Clause OR Apache-2.0`), so all pass the allowed list. `cargo deny
  check advisories` on the probe crate reports only bincode and yaml-rust,
  both from syntect; nothing for the image crates.
- Deterministic: each output is byte-identical to a second run in the same
  process (assert in the workload, all four files, both sets, all three
  platforms). Output sizes are also identical across macOS arm64, Linux x86_64
  and Linux arm64 for both sets (`image`: JPEG 320,299 bytes, PNG 566,972; separate:
  277,076 and 122,006), which suggests the resize is the same on every platform
  but is not a hash comparison; hashes were not compared. `fast_image_resize`
  picks AVX2 on x86_64 and NEON on arm64 at run time, and the sizes still
  matched. A different crate version or a different resize filter changes the
  bytes.
- Output size: the `image` PNG encoder produced 566,972 bytes for a screenshot
  the separate set's encoder wrote as 122,006 (neither encoder was tuned, and
  the two paths differ in more than the crate). Do not read this as one crate
  compressing better.

Glue: with the `image` crate the workload is about 25 lines. With separate
crates it is about 80 (format sniffing, a decoder per format, colour type
handling, dimension arithmetic).

### Undecodable inputs (raw: `raw-try-image-crate.txt`, macOS, `image` crate)

| File | Result |
|---|---|
| CMYK JPEG (4 components, made with `sips`) | decodes, comes out as `Rgb8`. Colour accuracy was not checked. |
| 4000 random bytes named `.jpg` | error: "The image format could not be determined" |
| JPEG header then zeros | error: "Format error decoding Jpeg: Found a marker with invalid length:0" |
| 16-bit RGB PNG | decodes as `Rgb16` |
| GIF (static) | decodes as `Rgba8` (first frame) |
| WebP (lossless) | decodes as `Rgb8` |

The Linux run of this table failed for the first three files because they were
made by hand on macOS and are not written by `gen`; only the macOS results are
recorded.

## The limits

Documentation, then what the live request showed. Live results are for the model
named, on 2026-09-29. Raw files are `raw/<vendor>-<case>.json`; request bodies
are not stored.

### anthropic-messages

Source: https://platform.claude.com/docs/en/build-with-claude/vision.

- Largest dimension: 8000 px. Above 20 images in one request, 2000 px per
  image (all image blocks count, including earlier turns and images inside
  `tool_result`).
- Largest encoded size: 10 MB base64 per image on the Claude API (5 MB on
  Bedrock and Google Cloud). Request limit 32 MB.
- Images per request: 600, or 100 for models with a 200k-token context.
- Downscaling: the API downscales an image larger than the model's long-edge or
  visual-token limit, keeping the aspect ratio: 2576 px and 4784 tokens for
  Claude 4.7 and later, 1568 px and 1568 tokens for other models. A
  `tool_result` image over the limit is rejected, not downscaled, for the
  computer-use and browser-use toolsets. Formats: JPEG, PNG, GIF, WebP;
  animation not supported, first frame used.
- Live (`claude-sonnet-5-5`):
  - 8000x6000 PNG: 200, billed `input_tokens` 4,788, which is the 4,784
    visual-token cap plus the prompt. The vendor downscaled it.
    (`raw/anthropic-flat-8000x6000.json`)
  - 9000x9000 PNG: 400 `invalid_request_error`, "At least one of the image
    dimensions exceed max allowed size: 8000 pixels".
    (`raw/anthropic-flat-9000x9000.json`)
  - 23 MB base64 PNG: 400, "image exceeds 10 MB maximum: 23045044 bytes >
    10485760 bytes". (`raw/anthropic-noise-2400.json`)
  - GIF: 200. (`raw/anthropic-small-gif.json`)
- Not reached: the 20-image 2000 px rule, the 100 and 600 image counts, the
  32 MB request limit (each needs many requests or a large body).

### openai-responses and openai-completions

Source: https://developers.openai.com/api/docs/guides/images-vision.

- The page lists, for the gpt-5.6 family (sol, terra, luna): `high` detail fits
  within 2048x2048 and 2,500 patches; `original` keeps dimensions up to 65,535
  px and rejects over 30,000 patches; `low` fits 512x512. It lists 30,000
  patches per image, 1,500 images per request and 512 MB per request, and PNG,
  JPEG, WebP and non-animated GIF.
- Live (`gpt-6-luna`, `detail` not set):
  - 8000x6000 and 9000x9000 PNG, both protocols: 400, "requires 47000 patches
    after processing, exceeding the limit of 30000" (9000x9000: 79,524 patches).
    A patch is a 32 px square: 250x188 = 47,000. The vendor does not downscale
    at the default detail; it rejects.
    (`raw/openai-flat-*.json`, `raw/openai-chat-flat-*.json`)
  - 23 MB base64 (2400x2400): 200 on both protocols, 6,783 and 6,782 input
    tokens. (`raw/openai-noise-2400.json`, `raw/openai-chat-noise-2400.json`)
  - GIF: 200 on the Responses protocol. Chat completions was not run for it.
- Not reached: the 512 MB request limit, the 1,500 images per request, and the
  `detail: high` path (a request that sets it).

### google-generative-ai

Source: https://ai.google.dev/gemini-api/docs/image-understanding.

- Documented: 3,600 image files per request; 20 MB total inline request size;
  PNG, JPEG, WebP, HEIC, HEIF (GIF is not listed); an image over 384 px on
  both sides is tiled into 768x768 tiles of 258 tokens each. No maximum
  dimension and no per-image byte limit is stated.
- Live (`gemini-3.1-flash-lite`):
  - 8000x6000 and 9000x9000 PNG: 200, 1,091 and 1,116 prompt tokens. The vendor
    resized them; the model answered 1024x768 and 500x500 (guesses).
    (`raw/google-flat-*.json`)
  - 23 MB base64 in one request: 200, 1,116 prompt tokens. This is over the
    documented 20 MB and was accepted. (`raw/google-noise-2400.json`)
  - GIF: 200, and the model gave the right size. GIF is not in the documented
    list. (`raw/google-small-gif.json`)
- `gemini-3.1-flash-lite` bills about 1,100 tokens whatever the input size, so
  its token count says nothing about the tile rule above.

### What this means for a common cap

pi's 2000x2000 and codex's 2048 fall under every dimension limit found (Anthropic
8000 and 2000 per image in a many-image request, OpenAI 30,000 patches and the
2,500-patch `high` cap). A 2000x2000 image is 3,969 patches of 32 px, which is
over OpenAI's 2,500-patch `high` cap; per the documentation OpenAI
would downscale it at `high` detail; the live request at the default detail
rejected larger images and did not downscale. pi's 4.5 MB base64 cap is under Anthropic's
10 MB. No probe here shows a vendor rejecting an image below 2000x2000 and 4.5 MB.

## Ticket bullets

- Keeps the aspect ratio, never enlarges: both sets did (the workload only
  resizes when a side exceeds 2000). The re-encode format is a choice (below).
- Byte-for-byte passthrough within limits: possible with either crate, since the
  decision only needs the dimensions and the byte length. Reading the
  dimensions from the header without decoding was not tried here. This is a
  choice (below).
- Undecodable: see the table above. `image` returns an error for a corrupt
  file and decodes a CMYK JPEG, so "refuse a CMYK JPEG" would be Fiber's own
  rule, not the crate's.
- Where the resized bytes live: the same process gives identical bytes and the
  output sizes match across three platforms, but identical bytes across crate
  versions are not promised, and Fiber pins `Cargo.lock` per release only. A
  choice (below).

## For the owner

1. Memory. Decoding a 12-megapixel image peaks at 145,000 KiB (`image`) or
   70,000 KiB (separate crates) on Linux x86_64, over the 24 MiB busy-session
   budget (`docs/performance.md`) either way, and an 81 MP file (a 1.2 MB PNG)
   reaches 535,000 KiB or 308,000 KiB. So admitting an image decoder to the
   session process means a budget ruling (an exception for the moment of a
   decode, or a limit on pixel count checked from the header before decoding),
   or running the decode outside the session process (the terminal is its own
   process, `docs/architecture.md`; a helper process is not designed). JPEG
   decoding at 1/2, 1/4 or 1/8 scale reduces this and `jpeg-decoder` offers it;
   it was not measured. Per the admission rules in `docs/dependencies.md` I have
   not put `image` in the runtime table; it is under "Waiting on other
   decisions".
2. Crate. `image` (23 crates, 1.4 MiB binary, no C, about 25 lines of glue) or
   separate crates (28 crates, 5 MiB binary on x86_64, half the memory, about
   80 lines of glue and a hand-written format switch). The memory difference
   comes from the resizer. This probe did not check whether `image` can be made
   to use less.
3. Passthrough. Evidence: every vendor took a within-limit image (the small GIF,
   the 2400x2400 PNG on OpenAI and Google) without help, and passthrough keeps
   the artifact and the sent bytes the same, so a cached prefix survives a
   resume without depending on the resizer at all. The cost is that an image
   under the cap can still be many megabytes (Anthropic rejects over 10 MB).
4. Format after a resize. Evidence: the JPEG photo at quality 80 came out at
   320,299 bytes (`image`), the screenshot PNG at 566,972 (`image`) or 122,006
   (`png` crate). JPEG suits photographs and loses sharp text edges; PNG suits
   screenshots. Options: JPEG for a JPEG input and PNG for the rest (as the
   workload does), or by content.
5. Determinism. Evidence above. Storing the resized bytes in `artifacts/` and
   sending those on resume makes determinism irrelevant, at the cost of a
   second file per resized image; recomputing avoids the file and depends on
   the crate version staying fixed across a resume.
6. Undecodable. `read` fails with `unsupported_file` and the decoder's message,
   or the module sends the bytes as they are. Evidence: Anthropic accepted the
   GIF and would reject bytes it cannot parse (not probed); OpenAI and Google
   were not sent a corrupt file. The CMYK JPEG decodes here, so refusing it is a
   rule to add on purpose.
7. Gemini and GIF. Gemini's documentation omits GIF and the live request took
   one. If Fiber sends GIF to Gemini on the strength of one request, that is
   undocumented behaviour; converting to PNG is the documented route.

## Malformed replies

None seen. One note for #190: `gemini-3.1-flash-lite` answered 1024x768 and
500x500 for images of 8000x6000 and 9000x9000; those are guesses about the
resized image, not errors.
