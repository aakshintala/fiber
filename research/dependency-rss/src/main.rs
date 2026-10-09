//! Each feature runs a small, fixed workload shaped like Fiber's use of that
//! crate. With no features the program does nothing, and that is the baseline.
#![allow(unused)]
use std::hint::black_box;

#[cfg(any(feature = "image", feature = "image-fir"))]
mod image_timing;

fn text(lines: usize) -> String {
    (0..lines).map(|i| format!("line {i}: the quick brown fox_{i} jumps over 42 lazy dogs\n")).collect()
}

fn main() {
    #[cfg(any(feature = "image", feature = "image-fir"))]
    {
        if image_timing::handle_child_args() {
            return;
        }
        if image_timing::run_if_png_photos() {
            return;
        }
        if image_timing::run_if_timing() {
            return;
        }
    }
    #[cfg(feature = "serde_json")]
    {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Event { kind: String, session_id: String, ts: u64, text: String }
        let events: Vec<Event> = (0..100)
            .map(|i| Event { kind: "assistant_delta".into(), session_id: "s1".into(), ts: i, text: "x".repeat(100) })
            .collect();
        let lines: Vec<String> = events.iter().map(|e| serde_json::to_string(e).unwrap()).collect();
        let back: Vec<Event> = lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        black_box(back);
    }
    #[cfg(feature = "ureq")]
    {
        // One real HTTPS request through the OS trust store, over the custom
        // connector Fiber uses to keep the socket for cancellation
        // (docs/architecture.md, "Cancellation").
        let slot: stash::Slot = Default::default();
        use ureq::unversioned::transport::Connector;
        let connector = stash::StashConnector { slot: slot.clone() }
            .chain(ureq::unversioned::transport::RustlsConnector::default());
        let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        let config = ureq::config::Config::builder().tls_config(tls).build();
        let agent = ureq::Agent::with_parts(config, connector, ureq::unversioned::resolver::DefaultResolver::default());
        let body = agent.get("https://example.com").call().unwrap().into_body().read_to_string().unwrap();
        assert!(slot.lock().unwrap().is_some(), "the connector kept the socket");
        black_box(body);
    }
    #[cfg(feature = "ratatui")]
    {
        use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
        let mut t = Terminal::new(TestBackend::new(200, 50)).unwrap();
        let s = text(60);
        for _ in 0..10 {
            t.draw(|f| f.render_widget(Paragraph::new(s.as_str()), f.area())).unwrap();
        }
    }
    #[cfg(feature = "crossterm")]
    {
        // Needs a terminal: run.sh runs this one under script(1).
        use crossterm::{event, terminal};
        terminal::enable_raw_mode().unwrap();
        let (w, h) = terminal::size().unwrap();
        while event::poll(std::time::Duration::from_millis(50)).unwrap() {
            black_box(event::read().unwrap());
        }
        terminal::disable_raw_mode().unwrap();
        black_box((w, h));
    }
    #[cfg(feature = "image")]
    {
        // Fiber's read tool on a photo and a screenshot: decode, fit inside
        // 2000x2000 keeping the aspect ratio (never enlarging), re-encode.
        // Fixtures come from research/image-limits/gen. Each file is resized
        // twice and the bytes compared.
        use image::{DynamicImage, ImageEncoder, codecs::{jpeg::JpegEncoder, png::PngEncoder}, imageops::FilterType};
        let dir = std::env::var("IMAGE_FIXTURES").unwrap_or_else(|_| "../image-limits/fixtures".into());
        let fit = |bytes: &[u8]| -> Vec<u8> {
            let img = image::load_from_memory(bytes).unwrap();
            let img = if img.width() > 2000 || img.height() > 2000 { img.resize(2000, 2000, FilterType::Lanczos3) } else { img };
            let mut out = Vec::new();
            match image::guess_format(bytes).unwrap() {
                image::ImageFormat::Jpeg => JpegEncoder::new_with_quality(&mut out, 80).encode_image(&DynamicImage::ImageRgb8(img.to_rgb8())).unwrap(),
                _ => img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap(),
            }
            out
        };
        let only = std::env::var("IMAGE_ONLY").unwrap_or_default();
        for name in ["photo-4000x3000.jpg", "shot-4000x3000.png", "small.gif", "small.webp", "flat-9000x9000.png"].into_iter().filter(|n| n.contains(&only) && (only.contains("flat") || !n.contains("flat"))) {
            let bytes = std::fs::read(format!("{dir}/{name}")).unwrap();
            let (a, b) = (fit(&bytes), fit(&bytes));
            assert!(a == b, "resize of {name} is not deterministic");
            eprintln!("{name}: {} -> {} bytes, deterministic", bytes.len(), a.len());
            black_box(a);
        }
    }
    #[cfg(feature = "image-parts")]
    {
        // The same workload without the image crate: one decoder per format,
        // fast_image_resize, and the JPEG and PNG encoders.
        use fast_image_resize::{PixelType, ResizeAlg, ResizeOptions, Resizer, FilterType, images::Image};
        let dir = std::env::var("IMAGE_FIXTURES").unwrap_or_else(|_| "../image-limits/fixtures".into());
        // Decode to (width, height, RGB8).
        let decode = |b: &[u8]| -> (u32, u32, Vec<u8>) {
            if b.starts_with(&[0xFF, 0xD8]) {
                let mut d = zune_jpeg::JpegDecoder::new(zune_jpeg::zune_core::bytestream::ZCursor::new(b));
                let px = d.decode().unwrap();
                let i = d.info().unwrap();
                (i.width as u32, i.height as u32, px)
            } else if b.starts_with(b"\x89PNG") {
                let mut dec = png::Decoder::new(std::io::Cursor::new(b));
                dec.set_transformations(png::Transformations::EXPAND);
                let mut r = dec.read_info().unwrap();
                let mut buf = vec![0; r.output_buffer_size().unwrap()];
                let info = r.next_frame(&mut buf).unwrap();
                assert_eq!(info.color_type, png::ColorType::Rgb);
                (info.width, info.height, buf)
            } else if b.starts_with(b"GIF8") {
                let mut o = gif::DecodeOptions::new();
                o.set_color_output(gif::ColorOutput::RGBA);
                let mut r = o.read_info(std::io::Cursor::new(b)).unwrap();
                let f = r.read_next_frame().unwrap().unwrap();
                let rgb = f.buffer.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
                (f.width as u32, f.height as u32, rgb)
            } else {
                let mut r = image_webp::WebPDecoder::new(std::io::Cursor::new(b)).unwrap();
                let (w, h) = r.dimensions();
                let mut buf = vec![0; r.output_buffer_size().unwrap()];
                r.read_image(&mut buf).unwrap();
                let rgb = if r.has_alpha() { buf.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect() } else { buf };
                (w, h, rgb)
            }
        };
        let fit = |b: &[u8]| -> Vec<u8> {
            let (w, h, px) = decode(b);
            let (nw, nh) = if w > 2000 || h > 2000 {
                let s = (2000.0 / w as f64).min(2000.0 / h as f64);
                (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
            } else { (w, h) };
            let src = Image::from_vec_u8(w, h, px, PixelType::U8x3).unwrap();
            let mut dst = Image::new(nw, nh, PixelType::U8x3);
            let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
            Resizer::new().resize(&src, &mut dst, &opts).unwrap();
            let mut out = Vec::new();
            if b.starts_with(&[0xFF, 0xD8]) {
                jpeg_encoder::Encoder::new(&mut out, 80).encode(dst.buffer(), nw as u16, nh as u16, jpeg_encoder::ColorType::Rgb).unwrap();
            } else {
                let mut e = png::Encoder::new(&mut out, nw, nh);
                e.set_color(png::ColorType::Rgb);
                e.set_depth(png::BitDepth::Eight);
                e.write_header().unwrap().write_image_data(dst.buffer()).unwrap();
            }
            out
        };
        let only = std::env::var("IMAGE_ONLY").unwrap_or_default();
        for name in ["photo-4000x3000.jpg", "shot-4000x3000.png", "small.gif", "small.webp", "flat-9000x9000.png"].into_iter().filter(|n| n.contains(&only) && (only.contains("flat") || !n.contains("flat"))) {
            let bytes = std::fs::read(format!("{dir}/{name}")).unwrap();
            let (a, b) = (fit(&bytes), fit(&bytes));
            assert!(a == b, "resize of {name} is not deterministic");
            eprintln!("{name}: {} -> {} bytes, deterministic", bytes.len(), a.len());
            black_box(a);
        }
    }
    #[cfg(feature = "image-fir")]
    {
        // Same workload as `image`, but resize with fast_image_resize (Lanczos3) instead of
        // imageops::resize. Uses fast_image_resize's optional `image` feature (IntoImageView).
        use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
        use image::{
            DynamicImage, GenericImageView, ImageBuffer, ImageEncoder, ImageFormat,
            codecs::{jpeg::JpegEncoder, png::PngEncoder},
        };
        let dir = std::env::var("IMAGE_FIXTURES").unwrap_or_else(|_| "../image-limits/fixtures".into());
        let empty_like = |w: u32, h: u32, src: &DynamicImage| -> DynamicImage {
            match src {
                DynamicImage::ImageLuma8(_) => DynamicImage::ImageLuma8(ImageBuffer::new(w, h)),
                DynamicImage::ImageLumaA8(_) => DynamicImage::ImageLumaA8(ImageBuffer::new(w, h)),
                DynamicImage::ImageRgb8(_) => DynamicImage::ImageRgb8(ImageBuffer::new(w, h)),
                DynamicImage::ImageRgba8(_) => DynamicImage::ImageRgba8(ImageBuffer::new(w, h)),
                DynamicImage::ImageLuma16(_) => DynamicImage::ImageLuma16(ImageBuffer::new(w, h)),
                DynamicImage::ImageLumaA16(_) => DynamicImage::ImageLumaA16(ImageBuffer::new(w, h)),
                DynamicImage::ImageRgb16(_) => DynamicImage::ImageRgb16(ImageBuffer::new(w, h)),
                DynamicImage::ImageRgba16(_) => DynamicImage::ImageRgba16(ImageBuffer::new(w, h)),
                DynamicImage::ImageRgb32F(_) => DynamicImage::ImageRgb32F(ImageBuffer::new(w, h)),
                DynamicImage::ImageRgba32F(_) => DynamicImage::ImageRgba32F(ImageBuffer::new(w, h)),
                _ => panic!("unsupported pixel layout"),
            }
        };
        let fit = |bytes: &[u8]| -> Vec<u8> {
            let img = image::load_from_memory(bytes).unwrap();
            let (w, h) = img.dimensions();
            let (nw, nh) = if w > 2000 || h > 2000 {
                let s = (2000.0 / w as f64).min(2000.0 / h as f64);
                (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
            } else {
                (w, h)
            };
            let img = if (nw, nh) == (w, h) {
                img
            } else {
                let mut dst = empty_like(nw, nh, &img);
                let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
                Resizer::new().resize(&img, &mut dst, &opts).unwrap();
                dst
            };
            let mut out = Vec::new();
            match image::guess_format(bytes).unwrap() {
                ImageFormat::Jpeg => {
                    JpegEncoder::new_with_quality(&mut out, 80)
                        .encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))
                        .unwrap();
                }
                _ => PngEncoder::new(&mut out)
                    .write_image(img.as_bytes(), img.width(), img.height(), img.color().into())
                    .unwrap(),
            }
            out
        };
        let only = std::env::var("IMAGE_ONLY").unwrap_or_default();
        for name in ["photo-4000x3000.jpg", "shot-4000x3000.png", "small.gif", "small.webp", "flat-9000x9000.png"].into_iter().filter(|n| n.contains(&only) && (only.contains("flat") || !n.contains("flat"))) {
            let bytes = std::fs::read(format!("{dir}/{name}")).unwrap();
            let (a, b) = (fit(&bytes), fit(&bytes));
            assert!(a == b, "resize of {name} is not deterministic");
            eprintln!("{name}: {} -> {} bytes, deterministic", bytes.len(), a.len());
            black_box(a);
        }
    }
    #[cfg(feature = "image-header")]
    {
        use image::ImageReader;
        let dir = std::env::var("IMAGE_FIXTURES").unwrap_or_else(|_| "../image-limits/fixtures".into());
        for name in ["photo-4000x3000.jpg", "shot-4000x3000.png", "small.gif", "small.webp", "flat-9000x9000.png"] {
            let path = format!("{dir}/{name}");
            let reader = ImageReader::open(&path).unwrap().with_guessed_format().unwrap();
            let format = reader.format().unwrap();
            let (w, h) = reader.into_dimensions().unwrap();
            eprintln!("{name}: {w}x{h} {format:?}");
            black_box((w, h));
        }
    }
    #[cfg(feature = "jsonschema")]
    {
        // A tool input schema of the shape Fiber's built-ins declare.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "offset": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1 },
                "mode": { "enum": ["read", "write"] }
            },
            "required": ["path"],
            "additionalProperties": false
        });
        let v = jsonschema::validator_for(&schema).unwrap();
        black_box(v.is_valid(&serde_json::json!({ "path": "src/main.rs", "offset": 10 })));
        black_box(v.iter_errors(&serde_json::json!({ "path": 3, "x": 1 })).count());
    }
    #[cfg(feature = "rusqlite")]
    {
        let path = std::env::temp_dir().join(format!("dep-rss-{}.db", std::process::id()));
        let mut db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "journal_mode", "WAL").unwrap();
        db.execute("create table s (id integer primary key, title text, ts integer)", []).unwrap();
        let tx = db.transaction().unwrap();
        for i in 0..100 {
            tx.execute("insert into s (title, ts) values (?1, ?2)", (format!("session {i}"), i)).unwrap();
        }
        tx.commit().unwrap();
        let n: i64 = db.query_row("select count(*) from s where title like '%9%'", [], |r| r.get(0)).unwrap();
        black_box(n);
        drop(db);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }
    #[cfg(feature = "mlua")]
    {
        let lua = mlua::Lua::new();
        let n: i64 = lua
            .load("local t = {} for i = 1, 100 do t[i] = { name = 'tool' .. i } end return #t")
            .eval()
            .unwrap();
        black_box(n);
    }
    #[cfg(feature = "clap")]
    {
        #[derive(clap::Parser)]
        struct Cli { #[command(subcommand)] cmd: Option<Cmd> }
        #[derive(clap::Subcommand)]
        enum Cmd {
            Ask { prompt: String }, Continue, Sessions,
            Session { #[command(subcommand)] cmd: SessionCmd },
            Auth, Models, Usage, Status, Doctor, Config { key: Option<String> }, Mcp, Permissions,
            Workspace, Upgrade, Serve, Remote, Install { name: String }, Update, List, Remove { name: String }, Approve,
        }
        #[derive(clap::Subcommand)]
        enum SessionCmd { Show { id: String }, List, Rename { id: String, title: String }, Remove { id: String }, Resume, Recover { id: String } }
        let cli = <Cli as clap::Parser>::parse_from(["fiber", "session", "rename", "abc", "new title"]);
        black_box(cli.cmd.is_some());
    }
    #[cfg(feature = "clap_complete")]
    {
        #[derive(clap::Parser)]
        struct Cli { #[command(subcommand)] cmd: Option<Cmd> }
        #[derive(clap::Subcommand)]
        enum Cmd {
            Ask { prompt: String }, Continue, Sessions,
            Session { #[command(subcommand)] cmd: SessionCmd },
            Auth, Models, Usage, Status, Doctor, Config { key: Option<String> }, Mcp, Permissions,
            Workspace, Upgrade, Serve, Remote, Install { name: String }, Update, List, Remove { name: String }, Approve,
        }
        #[derive(clap::Subcommand)]
        enum SessionCmd { Show { id: String }, List, Rename { id: String, title: String }, Remove { id: String }, Resume, Recover { id: String } }
        let mut cmd = <Cli as clap::CommandFactory>::command();
        for shell in [clap_complete::Shell::Bash, clap_complete::Shell::Zsh, clap_complete::Shell::Fish] {
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut cmd, "fiber", &mut out);
            black_box(out.len());
        }
    }
    #[cfg(feature = "thiserror")]
    {
        #[derive(Debug, thiserror::Error)]
        enum E { #[error("provider {0} returned {1}")] Provider(String, u16) }
        black_box(E::Provider("openrouter".into(), 429).to_string());
    }
    #[cfg(feature = "signal-hook")]
    {
        let mut s = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]).unwrap();
        black_box(s.pending().count());
    }
    #[cfg(feature = "getrandom")]
    {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).unwrap();
        black_box(b);
    }
    #[cfg(feature = "base64")]
    {
        use base64::Engine;
        black_box(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vec![7u8; 4096]));
    }
    #[cfg(feature = "ring")]
    {
        black_box(ring::digest::digest(&ring::digest::SHA256, &vec![7u8; 4096]));
    }
    #[cfg(feature = "rustix")]
    {
        let fd = rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY).unwrap();
        rustix::pty::grantpt(&fd).unwrap();
        rustix::pty::unlockpt(&fd).unwrap();
        black_box(rustix::pty::ptsname(&fd, Vec::new()).unwrap());
    }
    #[cfg(feature = "regex")]
    {
        let re = regex::Regex::new(r"\w+_\d{2,}\s+jumps").unwrap();
        black_box(re.find_iter(&text(200)).count());
    }
    #[cfg(feature = "ignore")]
    {
        // The probe's own directory, including target/, is the tree walked.
        let n = ignore::WalkBuilder::new(env!("CARGO_MANIFEST_DIR")).build().filter_map(Result::ok).count();
        black_box(n);
    }
    #[cfg(feature = "search")]
    {
        // Walk the probe's own directory, including target/, as `grep -rn`
        // would, and search every file for a pattern with a literal part.
        use grep_regex::RegexMatcher;
        use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
        let matcher = RegexMatcher::new_line_matcher(r"fn\s+\w+_\d+|serde").unwrap();
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .line_number(true)
            .build();
        let mut hits = 0usize;
        for entry in ignore::WalkBuilder::new(env!("CARGO_MANIFEST_DIR")).build().filter_map(Result::ok) {
            if entry.file_type().is_some_and(|t| t.is_file()) {
                let _ = searcher.search_path(&matcher, entry.path(), UTF8(|_, line| {
                    hits += line.len();
                    Ok(true)
                }));
            }
        }
        black_box(hits);
    }
    #[cfg(feature = "similar")]
    {
        let a = text(200);
        let b = a.replace("fox_5", "cat_5");
        black_box(similar::TextDiff::from_lines(&a, &b).unified_diff().to_string());
    }
    #[cfg(feature = "html5ever")]
    {
        // web_fetch: a 64 KiB page through the tokenizer alone, counting
        // tokens in the sink, as Fiber's converter does without a tree.
        use html5ever::tokenizer::{
            BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
        };
        struct Count(std::cell::Cell<usize>);
        impl TokenSink for &Count {
            type Handle = ();
            fn process_token(&self, _token: Token, _line: u64) -> TokenSinkResult<()> {
                self.0.set(self.0.get() + 1);
                TokenSinkResult::Continue
            }
        }
        let page =
            "<div><p>Hello <a href=\"/x\">world</a> &amp; friends</p></div>".repeat(1_500);
        let count = Count(std::cell::Cell::new(0));
        let tokenizer = Tokenizer::new(&count, TokenizerOpts::default());
        let queue = BufferQueue::default();
        queue.push_back(page.into());
        let _ = tokenizer.feed(&queue);
        tokenizer.end();
        black_box(count.0.get());
    }
    #[cfg(feature = "encoding_rs")]
    {
        // web_fetch: decoding a 64 KiB windows-1252 page by its declared
        // character set.
        let bytes = vec![0xe9u8; 64 * 1024];
        let (text, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
        black_box(text.len());
    }
    #[cfg(feature = "flate2")]
    {
        // The release install step: a 4 MiB ustar-like stream gzipped in
        // process, a block at a time, then streamed from memory through the
        // decoder, as the unpacker reads a downloaded archive.
        use std::io::Write;
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let page = text(40);
        let mut written = 0;
        while written < 4 * 1024 * 1024 {
            let mut member = vec![0u8; 512];
            member[..12].copy_from_slice(b"docs/page.md");
            member[257..263].copy_from_slice(b"ustar\0");
            member.extend_from_slice(page.as_bytes());
            member.resize(member.len().next_multiple_of(512), 0);
            encoder.write_all(&member).unwrap();
            written += member.len();
        }
        let gz = encoder.finish().unwrap();
        let mut decoder = flate2::bufread::GzDecoder::new(&gz[..]);
        black_box(std::io::copy(&mut decoder, &mut std::io::sink()).unwrap());
    }
    #[cfg(feature = "pulldown-cmark")]
    {
        let md = "# Heading\n\nSome *emphasis* and `code`.\n\n- item\n- item\n\n```rust\nfn main() {}\n```\n\n".repeat(20);
        black_box(pulldown_cmark::Parser::new(&md).count());
    }
    #[cfg(feature = "jiff")]
    {
        // The terminal's time of day under a prompt bubble: the system
        // zone, read once, formatting times of day.
        let zone = jiff::tz::TimeZone::system();
        let start = jiff::Timestamp::from_millisecond(1791468900000).unwrap();
        let mut shown = Vec::new();
        for i in 0..1000 {
            let ts = start.as_millisecond().saturating_add(i * 60_000);
            let ts = jiff::Timestamp::from_millisecond(ts).unwrap();
            let zoned = ts.to_zoned(zone.clone());
            shown.push(zoned.strftime("%H:%M").to_string());
        }
        black_box(shown);
    }
    #[cfg(feature = "lopdf")]
    {
        // The image child: build a 20-page PDF in memory, load it,
        // count its pages, cut pages 3 to 7, and save the cut to a `Vec`.
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let info_id = doc.add_object(dictionary! {
            "Title" => Object::string_literal("probe"),
            "CreationDate" => Object::string_literal("D:19700101000000Z"),
        });
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });
        let mut kids = Vec::new();
        for n in 1..=20u32 {
            let text = format!("page {n}");
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 48.into()]),
                    Operation::new("Td", vec![100.into(), 600.into()]),
                    Operation::new("Tj", vec![Object::string_literal(text.as_str())]),
                    Operation::new("ET", vec![]),
                ],
            };
            let content_id =
                doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            kids.push(Object::from(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            })));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => 20,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set("Info", info_id);
        let mut built = Vec::new();
        doc.save_to(&mut built).unwrap();
        let mut loaded = Document::load_mem(&built).unwrap();
        assert_eq!(loaded.get_pages().len(), 20);
        let remove: Vec<u32> = (1..=20u32).filter(|p| *p < 3 || *p > 7).collect();
        loaded.delete_pages(&remove);
        loaded.prune_objects();
        let mut cut = Vec::new();
        loaded.save_to(&mut cut).unwrap();
        assert_eq!(Document::load_mem(&cut).unwrap().get_pages().len(), 5);
        black_box(cut);
    }
    #[cfg(feature = "arborium")]
    {
        // The terminal highlights a reply's code blocks: one block in each
        // of three languages, each grammar loaded on its first block.
        let blocks = [
            ("rust", "fn main() {\n    let x = 1; // one\n    println!(\"{x}\");\n}\n"),
            ("python", "def main():\n    x = 1  # one\n    print(f\"{x}\")\n"),
            ("javascript", "function main() {\n  const x = 1; // one\n  console.log(`${x}`);\n}\n"),
        ];
        let mut hl = arborium::Highlighter::new();
        for (lang, code) in blocks {
            let code = code.repeat(20);
            black_box(hl.highlight_spans(lang, &code).unwrap().len());
        }
    }
    #[cfg(feature = "syntect")]
    {
        use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let mut h = HighlightLines::new(ss.find_syntax_by_extension("rs").unwrap(), &ts.themes["base16-ocean.dark"]);
        for l in text(100).lines() {
            black_box(h.highlight_line(l, &ss).unwrap());
        }
    }
}

/// The connector from research/concurrency/cancel_ureq_connector: it keeps a
/// clone of the TcpStream so another thread could shut it down.
#[cfg(feature = "ureq")]
mod stash {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};
    use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport};

    pub type Slot = Arc<Mutex<Option<TcpStream>>>;

    #[derive(Debug)]
    pub struct StashConnector {
        pub slot: Slot,
    }

    #[derive(Debug)]
    pub struct StashTransport {
        stream: TcpStream,
        buffers: LazyBuffers,
    }

    impl Connector<()> for StashConnector {
        type Out = StashTransport;
        fn connect(&self, details: &ConnectionDetails, _: Option<()>) -> Result<Option<StashTransport>, ureq::Error> {
            let addr = details.addrs.iter().copied().next().ok_or_else(|| std::io::Error::other("no address"))?;
            let stream = TcpStream::connect(addr)?;
            *self.slot.lock().unwrap() = Some(stream.try_clone()?);
            let buffers = LazyBuffers::new(details.config.input_buffer_size(), details.config.output_buffer_size());
            Ok(Some(StashTransport { stream, buffers }))
        }
    }

    impl Transport for StashTransport {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }
        fn transmit_output(&mut self, amount: usize, _: NextTimeout) -> Result<(), ureq::Error> {
            self.stream.write_all(&self.buffers.output()[..amount])?;
            Ok(())
        }
        fn await_input(&mut self, _: NextTimeout) -> Result<bool, ureq::Error> {
            let n = self.stream.read(self.buffers.input_append_buf())?;
            self.buffers.input_appended(n);
            Ok(n > 0)
        }
        fn is_open(&mut self) -> bool {
            true
        }
    }
}
