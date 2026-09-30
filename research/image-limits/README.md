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
    crates, so a separate set differs in the resizer, the encoders and the glue.
  - Workload: decode a 4000x3000 JPEG (quality 90, 3.6 MiB) and a 4000x3000 PNG
    (screenshot-like, 237 KiB); fit inside 2000x2000 keeping the aspect ratio
    (Lanczos3, never enlarging); re-encode the JPEG at quality 80 and the PNG as
    PNG; do each file twice and assert the two outputs are byte-identical. It
    also decodes a GIF and a WebP. Fixtures come from `gen/` (deterministic, not
    committed: `cd gen && cargo run --release -- ../fixtures`).
  - Figures are peak memory over an empty program: Linux peak RSS, macOS peak
    memory footprint. `raw/linux/*/rss.txt` holds one figure per case (the median
    of 5 runs, computed by `linux-probe/probe.sh`; the five samples were not
    saved). `raw/macos/rss.txt` holds all five samples per case. Linux ran on GitHub's
    `ubuntu-24.04` (AMD EPYC 7763) and `ubuntu-24.04-arm` runners through
    `linux-probe/probe.yml`, on a throwaway branch `probe/130-image` that is
    deleted. Run 36545862616. macOS is Apple M3 Pro.
- Limits: each vendor's own documentation, then one matrix of live requests per
  vendor with `live.py` (base64 in the request body, prompt "state the width and
  height"). Cases: `flat-8000x6000` (a 950 KB PNG, flat colour, cheap),
  `flat-9000x9000` (1.2 MB PNG), `small-gif` (a 400x300 GIF), `noise-2400`
  (2400x2400 random-byte PNG, 17.3 MB, 23.0 MB as base64). Each request holds
  one image. The raw files hold only `usage` counts, so they show the tokens each
  vendor billed and not the size it processed.

Prices and spend (input / output per million tokens, from each vendor's pricing
page, looked up 2026-09-29):

| Model | Price | Page | Spend |
|---|---|---|---:|
| claude-sonnet-5-5 | $2 / $10 | https://platform.claude.com/docs/en/about-claude/pricing | about $0.011 |
| gpt-6-luna | $0.10 / $0.50 (short context) | https://developers.openai.com/api/docs/pricing | about $0.002 (Responses and chat completions) |
| gemini-3.1-flash-lite | $0.25 / $1.50 | https://ai.google.dev/gemini-api/docs/pricing | about $0.001 |

The Gemini probe used `gemini-3.1-flash-lite`, not the `gemini-2.5-flash-lite`
the brief named: this key gets HTTP 404 "no longer available to new users" for
the 2.5 models (`raw/google-2.5-flash-lite-404.json`, one text-only request). Spend is the sum of each run's `usage` at those prices.

## The crate

Measured (raw: `raw/linux/out-*/rss.txt`, `determinism_*.txt`, `tree_*.txt`;
macOS: `raw/macos/`, run locally on macOS 26.6.2, Apple M3 Pro).

Peak over an empty program, all four files in sequence (peak is the largest
decode), KiB:

| Set | Linux x86_64 RSS | Linux arm64 RSS | macOS arm64 footprint | Crates | Stripped binary (x86_64 / arm64 / macOS) |
|---|---:|---:|---:|---:|---:|
| `image`, four codecs | 148,448 | 148,160 | 157,136 | 23 | 1,410 / 1,285 / 1,266 KiB |
| separate crates | 67,884 | 67,464 | 67,840 | 28 | 4,994 / 3,077 / 3,099 KiB |

The empty program is 323 KiB on Linux x86_64 and 331 KiB on macOS.
`image` is 1,087 KiB over baseline on Linux x86_64 (1,410 - 323), the separate
set 4,671 KiB.

By file (peak KiB, not over baseline; Linux x86_64 RSS): 4000x3000 JPEG
144,944 (`image`) and 69,876 (separate); 4000x3000 PNG 141,632 and 66,220;
GIF 4,128 and 4,876; WebP 4,872 and 5,324. An 81-megapixel PNG (9000x9000, a
1.2 MB file) peaks at 534,700 KiB in `image` and 307,988 KiB in the separate
set, on Linux x86_64 (macOS footprint: 533,456 and 305,680 KiB). Peak memory
follows pixel count, not file size, and a small file can be a decompression
bomb.

- Crates: `image` is 23 crates and the separate set is 28 (`raw/linux/*/tree_*.txt`).
  Five crates are only in `image`: `image`, `moxcms`, `pxfm`, `bytemuck` and
  `color_quant`. Ten are only in the separate set: `fast_image_resize`,
  `jpeg-encoder`, `document-features`, `litrs`, `proc-macro2`, `quote`, `syn`,
  `thiserror`, `thiserror-impl` and `unicode-ident`. The `document-features` chain
  is a build-time proc-macro dependency.
- C: none. `cargo tree` shows no `-sys` crate and no `cc` build dependency. The
  only build scripts are `crc32fast` and `num-traits`, which probe the
  compiler. All codecs are pure Rust. zune-jpeg uses `unsafe` for SIMD.
- Licences and advisories (`raw/cargo-deny.txt`, `deny.toml` is the allowed list
  from `docs/dependencies.md`): the `image` set passes; the only licence error
  is the probe crate itself, which has no licence field. The separate set also
  fails on `jpeg-encoder` 0.7.1, whose licence is `(MIT OR Apache-2.0) AND IJG`;
  IJG is not on the allowed list. `advisories ok` for both sets. (The earlier
  all-features check, which flagged bincode and yaml-rust from syntect, is not
  saved and is not relevant to these sets.)
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
- Live (`claude-sonnet-5-5`; one request per case, one image each):
  - 8000x6000 PNG: 200, billed `input_tokens` 4,788. The 4,784-token
    cap plus the prompt is 4,788; at 28 px patches the full image would be
    286 x 215 = 61,490 tokens. So the count is at the cap, not the full size.
    (`raw/anthropic-flat-8000x6000.json`)
  - One 9000x9000 PNG: 400 `invalid_request_error`, "At least one of the image
    dimensions exceed max allowed size: 8000 pixels".
    (`raw/anthropic-flat-9000x9000.json`)
  - One 23 MB base64 PNG: 400, "image exceeds 10 MB maximum: 23045044 bytes >
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
  - One 8000x6000 and one 9000x9000 PNG, on each protocol: 400, "requires 47000 patches
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
  - 8000x6000 and 9000x9000 PNG: 200, 1,091 and 1,116 prompt tokens, and the
    model answered 1024x768 and 500x500. The raw shows the token counts and
    the answers, not the size Google processed. (`raw/google-flat-*.json`)
  - 23 MB base64 in one request: 200, 1,116 prompt tokens. This is over the
    documented 20 MB and was accepted. (`raw/google-noise-2400.json`)
  - GIF: 200, and the model gave the right size. GIF is not in the documented
    list. (`raw/google-small-gif.json`)
- The four accepted Gemini requests were counted at 1,091 to 1,116 prompt tokens
  whatever the input size (a 400x300 GIF included), so those counts do not show
  the 768 px tile rule above.

### What this means for a common cap

pi's 2000x2000 falls within every dimension limit found (Anthropic 8000, and
2000 per image in a request of more than 20 images; OpenAI 30,000 patches). Codex's
2048 px is over the 2000 px many-image rule. A 2000x2000 image is 3,969 patches of 32 px, which is
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
   80 lines of glue and a hand-written format switch). The two paths differ in
   decode buffers, resizer and encoder, and the probe did not separate them, so
   it does not show which part costs the memory or whether `image` can be made
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

## Follow-up: where the image crate's memory goes (macOS)

Platform for every figure below: macOS 26.6.2 (build 25G83), Apple M3 Pro
(`Mac15,6`), Rust 1.98.1. Peak memory is the median of five runs of
`/usr/bin/time -l` peak memory footprint under `script(1)`, minus the empty
program, using `research/dependency-rss` (same fixtures and workload as above).
Raw numbers: `raw/macos-followup/`.

Probe features added for this pass:

- `image-fir`: `image` 0.25.10 decodes and encodes (four codecs,
  `default-features = false`); `fast_image_resize` 6.1.0 resizes with Lanczos3
  convolution. Resize uses fast_image_resize's optional `image` feature
  (`IntoImageView` / `IntoImageViewMut` on `DynamicImage`). Fit-inside-2000x2000
  arithmetic matches `image-parts`.
- `image-header`: `ImageReader::open` → `with_guessed_format` →
  `into_dimensions` on all five fixtures (no pixel decode).

Empty baseline on this machine: 1,008 KiB peak footprint (331 KiB stripped
binary).

All five fixtures in one process (peak is the costliest step), KiB over empty:

| Feature | Peak over empty (KiB) | Stripped binary (KiB) | Unique crates |
|---|---:|---:|---:|
| `image` | 157,120 | 1,266 | 23 |
| `image-parts` | 68,096 | 3,099 | 28 |
| `image-fir` | 72,352 | 3,088 | 32 |
| `image-header` | 3,888 | 954 | 23 |

Per fixture (`IMAGE_ONLY` selects one file), KiB over empty:

| Feature | photo-4000x3000.jpg | shot-4000x3000.png | flat-9000x9000.png |
|---|---:|---:|---:|
| `image` | 154,960 | 140,016 | 532,464 |
| `image-parts` | 66,592 | 62,960 | 304,672 |
| `image-fir` | 70,160 | 63,856 | 304,752 |

`image-header` over empty (all five headers in one run, including
`flat-9000x9000.png`): 3,888 KiB. Header-only output:
`raw/macos-followup/image-header-output.txt`.

On 12-megapixel photo and screenshot inputs, swapping `imageops::resize` for
`fast_image_resize` (`image-fir`) drops peak by about 85,000 KiB and lands
within a few thousand KiB of `image-parts`. On `flat-9000x9000.png`,
`image-fir` and `image-parts` match within 80 KiB; `image` stays about
228,000 KiB higher, so the extra cost there is not the resizer crate choice
alone but how the `image` resize path allocates while scaling.

### `image::Limits` defaults (image 0.25.10)

From `src/io/limits.rs`:

```rust
        Limits {
            max_image_width: None,
            max_image_height: None,
            max_alloc: Some(512 * 1024 * 1024),
        }
```

(`Default for Limits`, lines 49–56.) `max_image_width` and `max_image_height`
default to no limit; `max_alloc` defaults to 512 MiB.

`load_from_memory` and `ImageReader::decode` both use these defaults:
`ImageReader::new` sets `limits: Limits::default()` (`src/io/image_reader_type.rs`,
lines 89–94). `load_from_memory` builds a reader and calls `decode()`
(`src/images/dynimage.rs`, lines 1676–1679). `decode` clones that limit set,
calls `limits.reserve(decoder.total_bytes())?`, then `decoder.set_limits(limits)`
before `DynamicImage::from_decoder` (`src/io/image_reader_type.rs`, lines
314–320).

### Why `imageops::resize` costs more memory

`DynamicImage::resize` calls `imageops::resize` (`src/images/dynimage.rs`, lines
875–882, 892–898). For Lanczos3, `imageops::resize` runs a vertical pass into
a full-width `Rgba32FImage` (32-bit float per channel), then a horizontal pass
(`src/imageops/sample.rs`, lines 1008–1016). The vertical buffer is sized
`width × new_height` in `f32` RGBA (`vertical_sample`, lines 506–507), so
downscaling a 9000×9000 image keeps the original width through the first pass
and holds a large intermediate float buffer on top of the decoded pixels. The
`image-parts` and `image-fir` paths resize in 8-bit buffers via
`fast_image_resize` and do not build that two-pass `Rgba32F` pipeline.
