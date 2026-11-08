//! The core's thumbnail service (APP-038; Appendix H "Thumbnails"; P4b-R6, P4b-R12, P4b-R36):
//! the fixed tiers, the renderer registry (`image@1`, `icon@1`), the content-keyed PNG cache in
//! the per-workspace local data folder, the guards an untrusted file passes before any decode
//! (the size, the magic bytes, the header's dimensions, a JPEG's integrity), the decode
//! deadline and the bounded concurrency, the clean-up, and `app/thumbs.request`'s parameter
//! checks. Driven through `ThumbService` over a `WorkspaceHub` on a copy of the fixture
//! workspace (the fake clock and ids, QA-031), and through an in-process core for the
//! router. Images are made in the tests with the `image` crate's encoders.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
use lil_core::events::EventBus;
use lil_core::index::Index;
use lil_core::interchange::thumbs::{
    DECODED_EXTENSIONS, ICON, ImageRenderer, KEEP_UNTOUCHED, LiveSource, RenderError, Renderer,
    ThumbOptions, ThumbService, default_permits, default_renderers, entry_name, jpeg_complete,
    sniff, tier_px,
};
use lil_core::state::StateRegistry;
use lil_core::store::{WsPath, fsops};
use lil_core::tools::{ThumbState, Thumbnails, thumbs_request_px};
use lil_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_core::{CoreConfig, start_in_thread};
use lil_platform::fs::{FileIdentity, Probe, SystemProbe, VolumeInfo};
use lil_platform::{Clock, IdSource};
use lil_protocol::app::{ThumbItem, ThumbsRequestParams};
use lil_schema::MediaKind;
use lil_schema::display::{media_kind_of, renderable};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestApp, TestEnv, png, sha256_bytes};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const WORK: &str = "projects/sketchbook/assets/chr_paper_crane/work";
const SEALED: &str = "projects/sketchbook/assets/chr_paper_crane/v1.0.0/crane_b.png";

// ---------------------------------------------------------------------------------------------
// The allocation probe (ruling 5): the largest single allocation the probing thread makes.
// ---------------------------------------------------------------------------------------------

struct Probing;

static LARGEST: AtomicUsize = AtomicUsize::new(0);
/// Notes every thread's allocations, not just the probing one's.
static TRACK_ALL: AtomicBool = AtomicBool::new(false);

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
}

fn note(size: usize) {
    if TRACK_ALL.load(Ordering::Relaxed) || TRACK.try_with(Cell::get).unwrap_or(false) {
        LARGEST.fetch_max(size, Ordering::Relaxed);
    }
}

// SAFETY: every call is passed on to `System` unchanged; only the size is noted.
unsafe impl GlobalAlloc for Probing {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        // SAFETY: `ptr` came from this allocator (that is, `System`) with `layout`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator (that is, `System`) with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Probing = Probing;

/// `f`'s result and the largest single allocation this thread made while it ran.
fn largest_allocation<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.store(0, Ordering::Relaxed);
    TRACK.with(|t| t.set(true));
    let out = f();
    TRACK.with(|t| t.set(false));
    (out, LARGEST.load(Ordering::Relaxed))
}

/// `f`'s result and the largest single allocation any thread made while it ran.
async fn largest_allocation_anywhere<T>(f: impl std::future::Future<Output = T>) -> (T, usize) {
    LARGEST.store(0, Ordering::Relaxed);
    TRACK_ALL.store(true, Ordering::Relaxed);
    let out = f.await;
    TRACK_ALL.store(false, Ordering::Relaxed);
    (out, LARGEST.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------------------------------
// Images, made here (never committed).
// ---------------------------------------------------------------------------------------------

fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), format)
        .expect("the test image encodes");
    out
}

/// A `w` × `h` PNG with a pattern seeded by `seed`.
fn picture_seeded(w: u32, h: u32, seed: u8) -> Vec<u8> {
    let img = RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 251) as u8, (y % 241) as u8, seed, 255])
    });
    encode(DynamicImage::ImageRgba8(img), ImageFormat::Png)
}

fn picture(w: u32, h: u32) -> Vec<u8> {
    picture_seeded(w, h, 7)
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    });
    encode(DynamicImage::ImageRgb8(img), ImageFormat::Jpeg)
}

fn webp(w: u32, h: u32) -> Vec<u8> {
    let img = RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 256) as u8, 40, (y % 256) as u8, 255])
    });
    encode(DynamicImage::ImageRgba8(img), ImageFormat::WebP)
}

/// A PNG chunk with its CRC.
fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&png::crc32(&body).to_be_bytes());
    out
}

/// A well-formed PNG whose header claims `w` × `h` RGBA px, with a small IDAT (a zlib stream
/// of a few zero bytes), so a decoder would go on to allocate for the claimed size.
fn png_bomb(w: u32, h: u32) -> Vec<u8> {
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let raw = [0u8; 64];
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    zlib.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&png::adler32(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &zlib));
    out.extend(chunk(b"IEND", &[]));
    out
}

/// A JPEG whose frame header claims `w` × `h` px, with a scan of a few bytes.
fn jpeg_bomb(w: u16, h: u16) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    // SOF0: length 17, precision 8, height, width, 3 components.
    out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    // SOS: length 12, 3 components.
    out.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x0C, 3, 1, 0, 2, 0x11, 3, 0x11, 0, 0x3F, 0,
    ]);
    out.extend_from_slice(&[0x12, 0x34, 0x56, 0xFF, 0xD9]);
    out
}

/// A WebP (`VP8X`) whose canvas claims `w` × `h` px.
fn webp_bomb(w: u32, h: u32) -> Vec<u8> {
    let mut data = vec![0u8; 4];
    data.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
    data.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
    let mut body = b"WEBP".to_vec();
    body.extend_from_slice(b"VP8X");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(&data);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// `main` (a JPEG) with an APP1 EXIF segment right after its SOI, carrying a whole small
/// JPEG as its thumbnail: that thumbnail's `FF DA` and `FF D9` come before the main scan.
fn with_exif_thumbnail(main: &[u8]) -> Vec<u8> {
    let thumb = jpeg(16, 16);
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&thumb);
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&main[2..]);
    out
}

// ---------------------------------------------------------------------------------------------
// The rig: a hub over a copy of the fixture workspace, registered as the default.
// ---------------------------------------------------------------------------------------------

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

struct Rig {
    env: TestEnv,
    ws: PathBuf,
    states: Arc<StateRegistry>,
    hub: Arc<WorkspaceHub>,
    events: Arc<EventBus>,
    clock: Arc<FakeClock>,
    thumbs: Arc<ThumbService>,
}

impl Rig {
    fn new(options: ThumbOptions) -> Rig {
        Rig::with(options, true)
    }

    /// A rig whose workspace becomes the default only when `default` (else later, with
    /// [`Rig::make_default`]).
    fn with(options: ThumbOptions, default: bool) -> Rig {
        Rig::with_probe(options, default, Arc::new(SystemProbe))
    }

    /// [`Rig::with`], the store (and the service) seeing the disk through `probe`.
    fn with_probe(options: ThumbOptions, default: bool, probe: Arc<dyn Probe>) -> Rig {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("thumbs");
        if default {
            env.register_workspace(&ws, true);
        }
        let states = Arc::new(
            StateRegistry::open(
                &env.env,
                Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
            )
            .expect("the suite state.db"),
        );
        let clock = FakeClock::new();
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let events = Arc::new(EventBus::new(dyn_clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            dyn_clock,
            ids,
            lil_core::store::FailPoints::none(),
            probe,
            lil_core::watch::WatchConfig::default(),
        );
        let thumbs = ThumbService::new(hub.clone(), events.clone(), options);
        Rig {
            env,
            ws,
            states,
            hub,
            events,
            clock,
            thumbs,
        }
    }

    /// Registers the rig's workspace as the default, as a switch does.
    fn make_default(&self) {
        self.env.register_workspace(&self.ws, true);
        self.hub.refresh_default();
    }

    /// Another service over the same hub.
    fn service(&self, options: ThumbOptions) -> Arc<ThumbService> {
        ThumbService::new(self.hub.clone(), self.events.clone(), options)
    }

    /// Writes `rel` in the workspace, as an outside program would.
    fn put(&self, rel: &str, bytes: &[u8]) {
        write_file(
            &rel.split('/').fold(self.ws.clone(), |p, c| p.join(c)),
            bytes,
        );
    }

    /// The default workspace, opened, once its index has finished its first reconcile.
    async fn open(&self) -> Arc<OpenWorkspace> {
        let open = self
            .hub
            .open(&ws_ref(&self.ws))
            .await
            .expect("the workspace opens");
        tokio::time::timeout(DEADLINE, open.index.ready())
            .await
            .expect("the first reconcile within the deadline");
        open
    }

    fn local(&self) -> PathBuf {
        self.env.workspace_local_dir(&self.ws)
    }

    /// The cache files of `sha256` in the workspace's local folder, by name.
    fn entries_of(&self, sha256: &str) -> Vec<String> {
        cache_files(&self.local(), sha256)
    }

    async fn shutdown(self) {
        let Rig {
            env, states, hub, ..
        } = self;
        hub.close_all().await;
        states.close();
        drop((hub, states));
        if let Err(left) = env.close() {
            panic!("the test's folder was left behind: {left:?}");
        }
    }
}

fn cache_files(local: &Path, sha256: &str) -> Vec<String> {
    let shard = local.join("thumbs").join(&sha256[..2]);
    let mut names: Vec<String> = std::fs::read_dir(&shard)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(sha256))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn item(path: &str) -> ThumbItem {
    ThumbItem {
        path: path.to_string(),
    }
}

fn ws_path(rel: &str) -> WsPath {
    WsPath::parse(rel).expect("a workspace path")
}

fn work(name: &str) -> String {
    format!("{WORK}/{name}")
}

/// Subscribes to topic `thumbs` on `events`.
async fn listen(events: &EventBus) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(256);
    events.attach(7, tx);
    events
        .subscribe(7, &["thumbs".to_string()], &BTreeMap::new())
        .await;
    rx
}

/// The next `thumbs` event of `kind` for `sha256`; others are skipped.
async fn next_event(rx: &mut mpsc::Receiver<String>, kind: &str, sha256: &str) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let line = tokio::time::timeout(left, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no {kind} event for {sha256} within {DEADLINE:?}"))
            .expect("the bus is open");
        let message: Value = serde_json::from_str(&line).unwrap();
        let data = &message["params"]["data"];
        if data["kind"] == kind && data["sha256"] == sha256 {
            return data.clone();
        }
    }
}

/// Sets `path`'s last write to `age` before `clock`'s now.
#[allow(clippy::disallowed_methods)] // opens the planted file to age it
fn age(path: &Path, clock: &FakeClock, age: Duration) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    file.set_modified(clock.utc_now() - age).unwrap();
}

/// Plants a cache entry of `sha256` (as a render would have filed it), `old` before now.
fn plant(local: &Path, clock: &FakeClock, sha256: &str, old: Duration) -> PathBuf {
    let path = local
        .join("thumbs")
        .join(&sha256[..2])
        .join(entry_name(sha256, "image", 1, 96));
    write_file(&path, &picture(4, 4));
    age(&path, clock, old);
    path
}

fn fast() -> ThumbOptions {
    ThumbOptions {
        inline_wait: Duration::ZERO,
        ..ThumbOptions::default()
    }
}

fn assert_ready_at(local: &Path, state: &ThumbState, sha256: &str, px: u32) -> PathBuf {
    assert_eq!(state.state, "ready", "{state:?}");
    assert_eq!(state.sha256.as_deref(), Some(sha256), "{state:?}");
    let file = state.file.clone().expect("a ready thumb names its file");
    assert_eq!(
        file,
        local
            .join("thumbs")
            .join(&sha256[..2])
            .join(entry_name(sha256, "image", 1, px)),
        "{state:?}"
    );
    file
}

fn assert_icon(state: &ThumbState) {
    assert_eq!(state.state, "icon", "{state:?}");
    assert_eq!(state.renderer, "icon@1", "{state:?}");
    assert!(state.file.is_none(), "an icon has no file: {state:?}");
}

// ---------------------------------------------------------------------------------------------
// Test renderers.
// ---------------------------------------------------------------------------------------------

/// `image@<version>`: `image@1`'s render under another version number.
struct Versioned(u32);

impl Renderer for Versioned {
    fn id(&self) -> &'static str {
        "image"
    }
    fn version(&self) -> u32 {
        self.0
    }
    fn renders(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Image
    }
    fn render(&self, bytes: &[u8], px: u32) -> Result<Vec<u8>, RenderError> {
        ImageRenderer.render(bytes, px)
    }
}

/// A renderer that panics on every image.
struct Panicking;

impl Renderer for Panicking {
    fn id(&self) -> &'static str {
        "image"
    }
    fn version(&self) -> u32 {
        1
    }
    fn renders(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Image
    }
    fn render(&self, _bytes: &[u8], _px: u32) -> Result<Vec<u8>, RenderError> {
        panic!("a decoder bug");
    }
}

/// `image@1`, counting how many of its renders run at once.
#[derive(Default)]
struct Counting {
    now: AtomicUsize,
    most: AtomicUsize,
}

impl Renderer for Counting {
    fn id(&self) -> &'static str {
        "image"
    }
    fn version(&self) -> u32 {
        1
    }
    fn renders(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Image
    }
    fn render(&self, bytes: &[u8], px: u32) -> Result<Vec<u8>, RenderError> {
        let running = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(running, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        let out = ImageRenderer.render(bytes, px);
        self.now.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

/// `image@1`, but its first render hangs until [`Gated::release`].
#[derive(Default)]
struct Gated {
    first: AtomicBool,
    open: Mutex<bool>,
    opened: Condvar,
    hung_done: AtomicBool,
}

impl Gated {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

impl Renderer for Gated {
    fn id(&self) -> &'static str {
        "image"
    }
    fn version(&self) -> u32 {
        1
    }
    fn renders(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Image
    }
    fn render(&self, bytes: &[u8], px: u32) -> Result<Vec<u8>, RenderError> {
        if !self.first.swap(true, Ordering::SeqCst) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
            drop(open);
            let out = ImageRenderer.render(bytes, px);
            self.hung_done.store(true, Ordering::SeqCst);
            return out;
        }
        ImageRenderer.render(bytes, px)
    }
}

/// `image@1`, but every render hangs until [`HangAll::release`]; counts how many started and
/// how many ran at once.
#[derive(Default)]
struct HangAll {
    open: Mutex<bool>,
    opened: Condvar,
    started: AtomicUsize,
    now: AtomicUsize,
    most: AtomicUsize,
}

impl HangAll {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

impl Renderer for HangAll {
    fn id(&self) -> &'static str {
        "image"
    }
    fn version(&self) -> u32 {
        1
    }
    fn renders(&self, kind: MediaKind) -> bool {
        kind == MediaKind::Image
    }
    fn render(&self, bytes: &[u8], px: u32) -> Result<Vec<u8>, RenderError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let running = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(running, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
        drop(open);
        self.now.fetch_sub(1, Ordering::SeqCst);
        ImageRenderer.render(bytes, px)
    }
}

fn with_renderer(renderer: Arc<dyn Renderer>) -> Vec<Arc<dyn Renderer>> {
    let mut all: Vec<Arc<dyn Renderer>> = vec![renderer];
    all.extend(default_renderers().into_iter().filter(|r| r.id() == ICON));
    all
}

// ---------------------------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------------------------

#[test] // spec: APP-038
fn tier_px_rounds_and_caps() {
    assert_eq!(tier_px(96, 1.0), Ok(96));
    assert_eq!(tier_px(256, 1.5), Ok(384));
    assert_eq!(tier_px(160, 1.33), Ok(213), "212.8 rounds to 213");
    assert_eq!(tier_px(96, 1.25), Ok(120));
    assert_eq!(tier_px(384, 2.5), Ok(960));
    assert_eq!(tier_px(512, 4.0), Ok(2048), "the largest is 2048");
    for tier in [0, 95, 100, 513, 1024] {
        assert!(tier_px(tier, 1.0).is_err(), "{tier}");
    }
    for scale in [0.0, 0.99, 4.01, f64::NAN, f64::INFINITY, -1.0] {
        assert!(tier_px(256, scale).is_err(), "{scale}");
    }
    // The request's rules: a tier, or a large_px instead; 1 to 500 items.
    let params =
        |n: usize, tier: Option<u32>, scale: Option<f64>, large: Option<u32>| ThumbsRequestParams {
            items: (0..n).map(|i| item(&format!("work/{i}.png"))).collect(),
            tier,
            scale,
            large_px: large,
        };
    assert_eq!(
        thumbs_request_px(&params(1, Some(256), None, None)),
        Ok(256)
    );
    assert_eq!(
        thumbs_request_px(&params(500, Some(256), Some(2.0), None)),
        Ok(512)
    );
    assert_eq!(
        thumbs_request_px(&params(1, None, None, Some(513))),
        Ok(513)
    );
    assert_eq!(
        thumbs_request_px(&params(1, None, None, Some(4096))),
        Ok(4096)
    );
    for bad in [
        params(0, Some(96), None, None),
        params(501, Some(96), None, None),
        params(1, None, None, None),
        params(1, Some(96), None, Some(600)),
        params(1, None, None, Some(512)),
        params(1, None, None, Some(4097)),
        params(1, Some(97), None, None),
        params(1, Some(96), Some(5.0), None),
    ] {
        assert!(thumbs_request_px(&bad).is_err(), "{bad:?}");
    }
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn thumbs_request_returns_ready_or_pending_then_ready() {
    // Each render waits first (the test override), and the request doesn't wait at all: the
    // first answer is `pending`.
    let r = Rig::new(ThumbOptions {
        slow: Some(Duration::from_millis(300)),
        ..fast()
    });
    let bytes = picture(1024, 512);
    let sha = sha256_bytes(&bytes);
    let path = work("big.png");
    r.put(&path, &bytes);
    let _open = r.open().await;
    let mut rx = listen(&r.events).await;
    let px = tier_px(256, 1.5).unwrap();

    let first = r.thumbs.request(&[item(&path)], px).await;
    assert_eq!(first.len(), 1);
    let thumb = &first[0];
    assert_eq!(thumb.state, "pending", "{thumb:?}");
    assert_eq!(thumb.path, path);
    assert_eq!(
        thumb.sha256.as_deref(),
        Some(sha.as_str()),
        "the index's hash"
    );
    assert_eq!(thumb.media_kind, "image");
    assert_eq!(thumb.renderer, "image@1");
    assert_eq!(thumb.px, 384);
    assert!(thumb.file.is_none());

    // Announced on `thumbs` as {kind: "ready", sha256, renderer, px, file}.
    let ready = next_event(&mut rx, "ready", &sha).await;
    assert_eq!(ready["renderer"], "image@1", "{ready}");
    assert_eq!(ready["px"], 384, "{ready}");
    let file = PathBuf::from(ready["file"].as_str().expect("a file"));
    let want = r
        .local()
        .join("thumbs")
        .join(&sha[..2])
        .join(entry_name(&sha, "image", 1, 384));
    assert_eq!(file, want);
    let cached = std::fs::read(&file).unwrap();
    assert_eq!(
        png::long_side(&cached),
        384,
        "the long side is the tier's px"
    );
    assert_eq!(png::size(&cached), (384, 192), "the aspect ratio is kept");

    // Asked again, it is ready at once, from the same entry; with time to wait, a new size is
    // ready in the same request.
    let again = r.thumbs.request(&[item(&path)], px).await;
    assert_ready_at(&r.local(), &again[0], &sha, 384);
    let waiting = r.service(ThumbOptions {
        inline_wait: DEADLINE,
        ..ThumbOptions::default()
    });
    let small = waiting.request(&[item(&path)], 96).await;
    assert_ready_at(&r.local(), &small[0], &sha, 96);
    // A smaller image is never scaled up.
    let tiny = picture(40, 20);
    r.put(&work("tiny.png"), &tiny);
    let state = waiting.ensure(&ws_path(&work("tiny.png")), None, 512).await;
    let file = assert_ready_at(&r.local(), &state, &sha256_bytes(&tiny), 512);
    assert_eq!(png::size(&std::fs::read(file).unwrap()), (40, 20));
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn same_bytes_share_one_cache_entry() {
    let r = Rig::new(fast());
    let bytes = picture(300, 200);
    let sha = sha256_bytes(&bytes);
    let paths = [
        work("a.png"),
        "projects/sketchbook/assets/chr_paper_crane/b.png".to_string(),
    ];
    for p in &paths {
        r.put(p, &bytes);
    }
    let _open = r.open().await;
    let mut rx = listen(&r.events).await;
    let items: Vec<ThumbItem> = paths.iter().map(|p| item(p)).collect();

    let first = r.thumbs.request(&items, 96).await;
    for t in &first {
        assert_eq!(t.state, "pending", "{t:?}");
        assert_eq!(t.sha256.as_deref(), Some(sha.as_str()));
    }
    next_event(&mut rx, "ready", &sha).await;
    let waiting = r.service(ThumbOptions {
        inline_wait: DEADLINE,
        ..ThumbOptions::default()
    });
    let again = waiting.request(&items, 96).await;
    let a = assert_ready_at(&r.local(), &again[0], &sha, 96);
    let b = assert_ready_at(&r.local(), &again[1], &sha, 96);
    assert_eq!(a, b, "same bytes, one cache entry");
    assert_eq!(r.entries_of(&sha), [entry_name(&sha, "image", 1, 96)]);
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn cache_key_includes_renderer_version() {
    let r = Rig::new(fast());
    let bytes = picture(64, 64);
    let sha = sha256_bytes(&bytes);
    let path = ws_path(&work("v.png"));
    r.put(path.as_str(), &bytes);
    let _open = r.open().await;

    let v1 = r.thumbs.ensure(&path, None, 96).await;
    assert_ready_at(&r.local(), &v1, &sha, 96);
    let v2 = r.service(ThumbOptions {
        renderers: with_renderer(Arc::new(Versioned(2))),
        ..fast()
    });
    let state = v2.ensure(&path, None, 96).await;
    assert_eq!(state.state, "ready", "{state:?}");
    assert_eq!(state.renderer, "image@2");
    let file = state.file.unwrap();
    assert_eq!(
        file.file_name().unwrap().to_string_lossy(),
        entry_name(&sha, "image", 2, 96),
        "a new version renders anew, beside the old entry"
    );
    assert_eq!(
        r.entries_of(&sha),
        [
            entry_name(&sha, "image", 1, 96),
            entry_name(&sha, "image", 2, 96)
        ]
    );
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn non_images_get_the_icon_state() {
    // Time to hash what the index doesn't hold yet: every answer is final.
    let r = Rig::new(ThumbOptions {
        inline_wait: DEADLINE,
        ..ThumbOptions::default()
    });
    let files: [(&str, &[u8], &str); 4] = [
        ("notes.txt", b"plain text notes", "text"),
        ("model.glb", b"glTF\x02\0\0\0", "model_3d"),
        // An image kind `image@1` doesn't decode: still the icon, never read for a decode.
        ("anim.gif", b"GIF89a\x01\0\x01\0\0\0\0;", "image"),
        ("clip.mp4", b"\0\0\0\x18ftypmp42", "video"),
    ];
    for (name, bytes, _) in &files {
        r.put(&work(name), bytes);
    }
    let _open = r.open().await;
    let items: Vec<ThumbItem> = files.iter().map(|(n, _, _)| item(&work(n))).collect();
    let states = r.thumbs.request(&items, tier_px(160, 1.0).unwrap()).await;
    for ((name, bytes, kind), state) in files.iter().zip(&states) {
        assert_icon(state);
        assert_eq!(state.media_kind, *kind, "{name}");
        assert_eq!(state.px, 160);
        assert_eq!(
            state.sha256.as_deref(),
            Some(sha256_bytes(bytes).as_str()),
            "{name}: the index's hash"
        );
    }
    // One without a known hash isn't read: nothing is drawn from it (`sha256: null`, "not
    // read or not hashed").
    r.put("inbox/in_0123456789abcdef/late.txt", b"later");
    let state = r
        .thumbs
        .ensure(&ws_path("inbox/in_0123456789abcdef/late.txt"), None, 96)
        .await;
    assert_icon(&state);
    assert_eq!(state.sha256, None);
    assert!(!r.local().join("thumbs").exists(), "nothing was cached");
    drop(_open);
    r.shutdown().await;
}

/// Ruling 5: the size and the header are checked before any decoder exists, so a header
/// claiming 60,000 × 60,000 px is refused while the largest allocation the render makes stays
/// tiny. The probe is a counting global allocator on the rendering thread; the control shows
/// it sees a real decode's buffers.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn bomb_header_is_refused_before_allocating() {
    const SMALL: usize = 64 * 1024;
    let bombs = [
        ("png", png_bomb(60_000, 60_000)),
        ("png side", png_bomb(16_385, 16)),
        ("jpeg", jpeg_bomb(60_000, 60_000)),
        ("webp", webp_bomb(60_000, 60_000)),
    ];
    for (what, bytes) in &bombs {
        assert!(sniff(bytes).is_some(), "{what}: a real signature");
        let (result, largest) = largest_allocation(|| ImageRenderer.render(bytes, 256));
        assert!(
            matches!(result, Err(RenderError::TooLarge(_))),
            "{what}: {result:?}"
        );
        assert!(
            largest < SMALL,
            "{what}: refused only after allocating {largest} bytes"
        );
    }
    // The control: a real 1024 × 1024 decode allocates its pixels, and the probe sees it.
    let real = picture(1024, 1024);
    let (result, largest) = largest_allocation(|| ImageRenderer.render(&real, 256));
    assert!(result.is_ok(), "{result:?}");
    assert!(largest >= 1024 * 1024 * 3, "the probe sees {largest} bytes");

    // Through the service: the icon, and nothing cached.
    let r = Rig::new(fast());
    let bomb = png_bomb(60_000, 60_000);
    r.put(&work("bomb.png"), &bomb);
    let _open = r.open().await;
    let state = r
        .thumbs
        .ensure(&ws_path(&work("bomb.png")), None, 256)
        .await;
    assert_icon(&state);
    assert_eq!(state.sha256.as_deref(), Some(sha256_bytes(&bomb).as_str()));
    assert!(r.entries_of(&sha256_bytes(&bomb)).is_empty());
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn truncated_and_empty_files_give_the_icon() {
    let r = Rig::new(fast());
    let whole = picture(200, 200);
    let main = jpeg(200, 100);
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("empty.png", Vec::new()),
        ("half.png", whole[..whole.len() / 2].to_vec()),
        ("header.png", png::header_only(64, 64)),
        ("signature.png", b"\x89PNG\r\n\x1a\n".to_vec()),
        ("cut.jpg", main[..main.len() * 7 / 10].to_vec()),
        ("noscan.jpg", main[..20].to_vec()),
        ("half.webp", {
            let w = webp(64, 64);
            w[..w.len() / 2].to_vec()
        }),
    ];
    for (name, bytes) in &files {
        r.put(&work(name), bytes);
        assert!(ImageRenderer.render(bytes, 96).is_err(), "{name}");
    }
    let _open = r.open().await;
    for (name, bytes) in &files {
        let state = r.thumbs.ensure(&ws_path(&work(name)), None, 96).await;
        assert_icon(&state);
        let sha = sha256_bytes(bytes);
        assert_eq!(state.sha256.as_deref(), Some(sha.as_str()), "{name}");
        assert!(r.entries_of(&sha).is_empty(), "{name}");
    }
    // A missing file: the icon, no hash.
    let state = r.thumbs.ensure(&ws_path(&work("gone.png")), None, 96).await;
    assert_icon(&state);
    assert_eq!(state.sha256, None);
    drop(_open);
    r.shutdown().await;
}

/// Ruling 4 (PF-24, PF-27): a JPEG cut inside its scan gets the icon even though its EXIF
/// thumbnail's `FF D9` is there; one with an EXIF thumbnail and valid trailing bytes is ready.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_jpeg_cut_in_its_scan_is_the_icon_and_one_with_an_exif_thumbnail_is_ready() {
    let main = jpeg(200, 100);
    let exif = with_exif_thumbnail(&main);
    let mut trailing = exif.clone();
    trailing.extend_from_slice(b"\0\0trailing bytes after the image");
    let cut = exif[..exif.len() - main.len() / 3].to_vec();
    assert!(jpeg_complete(&main));
    assert!(
        jpeg_complete(&trailing),
        "the last FF D9 is after the last FF DA"
    );
    assert!(
        !jpeg_complete(&cut),
        "the only FF D9 left is the thumbnail's"
    );
    assert!(!jpeg_complete(&main[..main.len() - 2]), "no end-of-image");

    let r = Rig::new(fast());
    r.put(&work("exif.jpg"), &trailing);
    r.put(&work("cut.jpg"), &cut);
    r.put(&work("plain.webp"), &webp(120, 60));
    let _open = r.open().await;
    let ready = r.thumbs.ensure(&ws_path(&work("exif.jpg")), None, 96).await;
    let file = assert_ready_at(&r.local(), &ready, &sha256_bytes(&trailing), 96);
    assert_eq!(png::size(&std::fs::read(file).unwrap()), (96, 48));
    let icon = r.thumbs.ensure(&ws_path(&work("cut.jpg")), None, 96).await;
    assert_icon(&icon);
    // WebP is decoded too.
    let webp_state = r
        .thumbs
        .ensure(&ws_path(&work("plain.webp")), None, 96)
        .await;
    assert_eq!(webp_state.state, "ready", "{webp_state:?}");
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn an_exe_named_png_gives_the_icon() {
    let mut exe = b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xff\xff\0\0".to_vec();
    exe.extend_from_slice(&[0u8; 200]);
    assert_eq!(sniff(&exe), None);
    assert_eq!(ImageRenderer.render(&exe, 96), Err(RenderError::NotAnImage));
    let r = Rig::new(fast());
    r.put(&work("setup.png"), &exe);
    // A real JPEG named .png is still an image: its magic bytes say so.
    let jpg = jpeg(50, 50);
    r.put(&work("photo.png"), &jpg);
    let _open = r.open().await;
    let state = r
        .thumbs
        .ensure(&ws_path(&work("setup.png")), None, 96)
        .await;
    assert_icon(&state);
    assert_eq!(state.media_kind, "image");
    assert!(r.entries_of(&sha256_bytes(&exe)).is_empty());
    let state = r
        .thumbs
        .ensure(&ws_path(&work("photo.png")), None, 96)
        .await;
    assert_ready_at(&r.local(), &state, &sha256_bytes(&jpg), 96);
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_panicking_decode_gives_the_icon() {
    let r = Rig::new(ThumbOptions {
        renderers: with_renderer(Arc::new(Panicking)),
        permits: 1,
        ..fast()
    });
    let a = picture(32, 32);
    let b = picture_seeded(32, 32, 9);
    r.put(&work("a.png"), &a);
    r.put(&work("b.png"), &b);
    let _open = r.open().await;
    for (name, bytes) in [("a.png", &a), ("b.png", &b)] {
        // The second one runs too: the panic released the only permit.
        let state =
            tokio::time::timeout(DEADLINE, r.thumbs.ensure(&ws_path(&work(name)), None, 96))
                .await
                .expect("answered");
        assert_icon(&state);
        assert_eq!(state.sha256.as_deref(), Some(sha256_bytes(bytes).as_str()));
    }
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn concurrency_is_bounded() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    assert_eq!(
        default_permits(),
        (cores / 2).clamp(1, 4),
        "min(4, cores / 2)"
    );
    assert_eq!(ThumbOptions::default().permits, default_permits());

    let counting = Arc::new(Counting::default());
    let r = Rig::new(ThumbOptions {
        renderers: with_renderer(counting.clone()),
        permits: 2,
        ..fast()
    });
    assert_eq!(
        r.service(ThumbOptions::default()).permits(),
        default_permits()
    );
    let names: Vec<String> = (0..8u8)
        .map(|i| {
            let name = work(&format!("c{i}.png"));
            r.put(&name, &picture_seeded(48, 48, i));
            name
        })
        .collect();
    let _open = r.open().await;
    let paths: Vec<WsPath> = names.iter().map(|n| ws_path(n)).collect();
    let all = futures_util::future::join_all(paths.iter().map(|p| r.thumbs.ensure(p, None, 96)));
    let states = tokio::time::timeout(DEADLINE, all).await.expect("answered");
    for s in &states {
        assert_eq!(s.state, "ready", "{s:?}");
    }
    let most = counting.most.load(Ordering::SeqCst);
    assert!(most <= 2, "{most} renders ran at once with 2 permits");
    assert!(most >= 1);
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_changed_file_is_filed_under_the_hash_it_was_read_with() {
    let r = Rig::new(fast());
    let old = picture(64, 64);
    let new = picture_seeded(64, 64, 200);
    let (old_sha, new_sha) = (sha256_bytes(&old), sha256_bytes(&new));
    let path = work("changed.png");
    r.put(&path, &old);
    let _open = r.open().await;
    let mut rx = listen(&r.events).await;
    // The file changes after the index read it: the caller still expects the old hash.
    r.put(&path, &new);

    let state = r.thumbs.ensure(&ws_path(&path), Some(&old_sha), 96).await;
    assert_eq!(
        state.state, "pending",
        "a differing hash answers pending: {state:?}"
    );
    assert_eq!(
        state.sha256.as_deref(),
        Some(new_sha.as_str()),
        "the hash read"
    );
    assert!(state.file.is_none());
    assert!(
        r.entries_of(&old_sha).is_empty(),
        "nothing is filed under the old hash"
    );
    assert_eq!(
        r.entries_of(&new_sha),
        [entry_name(&new_sha, "image", 1, 96)]
    );
    // The result is announced under the hash read.
    let ready = next_event(&mut rx, "ready", &new_sha).await;
    assert_eq!(ready["px"], 96, "{ready}");
    drop(_open);
    r.shutdown().await;
}

/// An expected hash comes from a manifest, a workspace file anyone can edit: one that isn't 64
/// hex digits never becomes part of a cache path (no traversal out of `thumbs\`, no panic on a
/// multi-byte first character); the file is hashed on demand instead. An upper-case one is
/// the same hash.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_malformed_expected_hash_is_hashed_on_demand() {
    let r = Rig::new(fast());
    let bytes = picture(32, 32);
    let sha = sha256_bytes(&bytes);
    let path = ws_path(&work("m.png"));
    r.put(path.as_str(), &bytes);
    let _open = r.open().await;
    for bad in [r"..\..\x", "../x", "€€", "", "zz", &"g".repeat(64)] {
        let state = r.thumbs.ensure(&path, Some(bad), 96).await;
        assert_ready_at(&r.local(), &state, &sha, 96);
    }
    let upper = r.thumbs.ensure(&path, Some(&sha.to_uppercase()), 96).await;
    assert_ready_at(&r.local(), &upper, &sha, 96);
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, I1: each item is answered with its own path, never with the path of an item
/// whose render it shares; two paths with the same bytes in one request.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038, CORE-168
async fn duplicate_bytes_in_one_request_answer_each_item_with_its_own_path() {
    let r = Rig::new(ThumbOptions {
        inline_wait: DEADLINE,
        ..ThumbOptions::default()
    });
    let bytes = picture(64, 48);
    let sha = sha256_bytes(&bytes);
    let paths = [work("dup_a.png"), work("dup_b.png")];
    for p in &paths {
        r.put(p, &bytes);
    }
    let _open = r.open().await;
    let items: Vec<ThumbItem> = paths.iter().map(|p| item(p)).collect();
    let states = r.thumbs.request(&items, 256).await;
    for (path, state) in paths.iter().zip(&states) {
        assert_eq!(
            &state.path, path,
            "each item echoes its own path: {state:?}"
        );
        assert_ready_at(&r.local(), state, &sha, 256);
    }
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, I1: `a` was changed (its caller still expects `x`); `b` really holds `x`. Asked
/// together, `a` answers `pending` with its new hash and `b` is `ready` under `x`, each under
/// its own path.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_stale_hash_equal_to_another_files_bytes_does_not_leak_between_items() {
    let r = Rig::new(ThumbOptions {
        slow: Some(Duration::from_millis(300)),
        ..fast()
    });
    let x = picture_seeded(40, 40, 11);
    let y = picture_seeded(40, 40, 12);
    let (sha_x, sha_y) = (sha256_bytes(&x), sha256_bytes(&y));
    let (a, b) = (ws_path(&work("stale_a.png")), ws_path(&work("holds_x.png")));
    r.put(a.as_str(), &x);
    r.put(b.as_str(), &x);
    let _open = r.open().await;
    r.put(a.as_str(), &y);
    let (sa, sb) = tokio::join!(
        r.thumbs.ensure(&a, Some(&sha_x), 96),
        r.thumbs.ensure(&b, Some(&sha_x), 96)
    );
    assert_eq!(sa.path, a.as_str(), "{sa:?}");
    assert_eq!(sa.state, "pending", "{sa:?}");
    assert_eq!(sa.sha256.as_deref(), Some(sha_y.as_str()), "{sa:?}");
    assert_eq!(sb.path, b.as_str(), "{sb:?}");
    assert_ready_at(&r.local(), &sb, &sha_x, 96);
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, I1 and ruling 2: the default workspace switches while a render of the same
/// path in the old one runs; the new workspace's item is answered from its own bytes and
/// cache.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038, ARCH-097
async fn a_default_switch_during_a_render_answers_from_the_new_workspace() {
    let r = Rig::new(ThumbOptions {
        slow: Some(Duration::from_millis(500)),
        ..fast()
    });
    let first = picture_seeded(50, 50, 21);
    let second = picture_seeded(50, 50, 22);
    let path = work("switch.png");
    r.put(&path, &first);
    let _open = r.open().await;
    let other = r.env.copy_fixture_workspace("other");
    write_file(
        &path.split('/').fold(other.clone(), |p, c| p.join(c)),
        &second,
    );
    let thumbs = r.thumbs.clone();
    let wp = ws_path(&path);
    let old = tokio::spawn(async move { thumbs.ensure(&wp, None, 96).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    r.env.register_workspace(&other, true);
    r.hub.refresh_default();
    let other_local = r.env.workspace_local_dir(&other);
    let state = r.thumbs.ensure(&ws_path(&path), None, 96).await;
    assert_ready_at(&other_local, &state, &sha256_bytes(&second), 96);
    let old = old.await.unwrap();
    assert_ready_at(&r.local(), &old, &sha256_bytes(&first), 96);
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, I2: a file the index doesn't hold is hashed before its decode waits, so its
/// `pending` answer carries the hash its `thumbs` event names. (`.git` is a folder the index
/// never enters.)
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn an_unindexed_pending_item_carries_the_hash_its_event_names() {
    let r = Rig::new(ThumbOptions {
        slow: Some(Duration::from_millis(400)),
        ..fast()
    });
    let bytes = picture(60, 30);
    let sha = sha256_bytes(&bytes);
    let path = work(".git/fresh.png");
    r.put(&path, &bytes);
    let open = r.open().await;
    assert_eq!(
        open.index.files_of_path(&ws_path(&path)).await.unwrap(),
        None
    );
    let mut rx = listen(&r.events).await;
    let states = r.thumbs.request(&[item(&path)], 96).await;
    assert_eq!(states[0].state, "pending", "{:?}", states[0]);
    assert_eq!(
        states[0].sha256.as_deref(),
        Some(sha.as_str()),
        "{:?}",
        states[0]
    );
    let ready = next_event(&mut rx, "ready", &sha).await;
    assert_eq!(ready["sha256"], sha.as_str());
    drop(open);
    r.shutdown().await;
}

/// Fix round 1, I3: the size guard comes before any buffer. A file over 256 MiB is only
/// hashed, in fixed chunks; one of exactly 256 MiB is read into one buffer of its size plus
/// one byte, never a doubled one. The probe watches every thread while `ensure` runs. The
/// files are made with `set_len` (no data written) and go with the test's folder.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
#[allow(clippy::disallowed_methods)] // makes the large files
async fn the_size_guard_comes_before_any_buffer() {
    const MIB: u64 = 1024 * 1024;
    let r = Rig::new(fast());
    for (name, len) in [("over.png", 300 * MIB), ("at.png", 256 * MIB)] {
        let file = work(name).split('/').fold(r.ws.clone(), |p, c| p.join(c));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&file).unwrap();
        // A PNG signature, then zeros: no header to read, so nothing is decoded.
        use std::io::Write as _;
        (&f).write_all(b"\x89PNG\r\n\x1a\n").unwrap();
        f.set_len(len).unwrap();
    }
    let _open = r.open().await;
    let (over, largest) =
        largest_allocation_anywhere(r.thumbs.ensure(&ws_path(&work("over.png")), None, 96)).await;
    assert_icon(&over);
    assert_eq!(over.sha256, None, "never hashed: {over:?}");
    assert!(
        largest < MIB as usize,
        "{largest} bytes for a file over the limit"
    );
    let (at, largest) =
        largest_allocation_anywhere(r.thumbs.ensure(&ws_path(&work("at.png")), None, 96)).await;
    assert_icon(&at);
    assert!(
        largest <= (256 * MIB + 1) as usize,
        "{largest} bytes for a file of exactly 256 MiB"
    );
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, M1: decodes that passed their deadline still hold their memory until their
/// thread ends, so new decodes wait while running plus abandoned ones reach twice the
/// permits. Their items answer `pending` meanwhile, then resolve once capacity returns.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn abandoned_decodes_are_capped_at_twice_the_permits() {
    let hang = Arc::new(HangAll::default());
    let r = Rig::new(ThumbOptions {
        renderers: with_renderer(hang.clone()),
        permits: 1,
        deadline: Duration::from_millis(100),
        ..fast()
    });
    // Released however the test ends, so no thread stays hung.
    struct Release(Arc<HangAll>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let _release = Release(hang.clone());
    let names: Vec<String> = (0..5u8)
        .map(|i| {
            let name = work(&format!("h{i}.png"));
            r.put(&name, &picture_seeded(24, 24, 100 + i));
            name
        })
        .collect();
    let _open = r.open().await;
    let jobs: Vec<_> = names
        .iter()
        .map(|n| {
            let (thumbs, path) = (r.thumbs.clone(), ws_path(n));
            tokio::spawn(async move { thumbs.ensure(&path, None, 96).await })
        })
        .collect();
    let until = Instant::now() + DEADLINE;
    while hang.started.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < until, "no decode started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Several deadlines pass: no third decode starts while two hang.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(hang.started.load(Ordering::SeqCst), 2, "2 x 1 permit");
    hang.release();
    let mut states = Vec::new();
    for job in jobs {
        states.push(tokio::time::timeout(DEADLINE, job).await.unwrap().unwrap());
    }
    assert!(hang.most.load(Ordering::SeqCst) <= 2);
    assert_eq!(
        states.iter().filter(|s| s.state == "icon").count(),
        2,
        "{states:?}"
    );
    assert_eq!(
        states.iter().filter(|s| s.state == "ready").count(),
        3,
        "{states:?}"
    );
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, M2: a cache name that isn't a plain file (a junction planted there) is a miss:
/// never served, never written through; the write is refused and the item gets the icon.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038, ARCH-112
#[allow(clippy::disallowed_methods)] // makes the junction and its target
async fn a_junction_at_a_cache_entry_is_a_miss() {
    let r = Rig::new(fast());
    let bytes = picture(32, 32);
    let sha = sha256_bytes(&bytes);
    let path = ws_path(&work("j.png"));
    r.put(path.as_str(), &bytes);
    let outside = r.env.root().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let entry = r
        .local()
        .join("thumbs")
        .join(&sha[..2])
        .join(entry_name(&sha, "image", 1, 96));
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    lil_platform::fs::create_junction(&entry, &outside).unwrap();
    let _open = r.open().await;
    for expected in [Some(sha.as_str()), None] {
        let state = r.thumbs.ensure(&path, expected, 96).await;
        assert_icon(&state);
        assert_eq!(state.sha256.as_deref(), Some(sha.as_str()));
    }
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing written through"
    );
    std::fs::remove_dir(&entry).unwrap();
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, M3: a cache hit counts as a touch, so an entry written 8 days ago and used
/// today survives clean-up even when nothing live holds its hash.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_cache_hit_counts_as_a_touch() {
    let r = Rig::new(fast());
    let bytes = picture(36, 36);
    let sha = sha256_bytes(&bytes);
    let path = ws_path(&work("touched.png"));
    r.put(path.as_str(), &bytes);
    let _open = r.open().await;
    let made = r.thumbs.ensure(&path, None, 96).await;
    let file = assert_ready_at(&r.local(), &made, &sha, 96);
    age(&file, &r.clock, 8 * DAY);
    let hit = r.thumbs.ensure(&path, Some(&sha), 96).await;
    assert_ready_at(&r.local(), &hit, &sha, 96);
    assert_eq!(r.thumbs.clean_up(&|_| false, true).await, 0);
    assert!(file.exists(), "used today");
    drop(_open);
    r.shutdown().await;
}

/// Fix round 1, M4: with no default workspace at start, clean-up's first pass runs once one
/// becomes the default and its index has finished its first reconcile; the next pass waits 24 h
/// on the core's clock (the fake one here), not on tokio's.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn clean_up_runs_after_the_default_appears_and_daily_on_the_core_clock() {
    let r = Rig::with(fast(), false);
    let stop = tokio_util::sync::CancellationToken::new();
    r.thumbs.spawn_clean_up(stop.clone());
    let local = r.local();
    let first = plant(&local, &r.clock, &sha256_bytes(b"first"), 8 * DAY);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(first.exists(), "no default workspace: no pass");
    r.make_default();
    let until = Instant::now() + DEADLINE;
    while first.exists() {
        assert!(
            Instant::now() < until,
            "no first pass after the default appeared"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let second = plant(&local, &r.clock, &sha256_bytes(b"second"), 8 * DAY);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        second.exists(),
        "the next pass waits 24 h on the core's clock"
    );
    r.clock.advance(DAY);
    let until = Instant::now() + DEADLINE;
    while second.exists() {
        assert!(
            Instant::now() < until,
            "no pass 24 h later on the core's clock"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.cancel();
    r.shutdown().await;
}

/// The real probe, but opening a file named `hung.png` to read it blocks until
/// [`HangingProbe::release`]: a read that stalls (a dying disk, a network volume).
#[derive(Debug, Default)]
struct HangingProbe {
    open: Mutex<bool>,
    opened: Condvar,
}

impl HangingProbe {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

impl Probe for HangingProbe {
    fn identity(&self, path: &Path) -> std::io::Result<FileIdentity> {
        SystemProbe.identity(path)
    }
    fn volume_info(&self, path: &Path) -> std::io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        if path.file_name().is_some_and(|n| n == "hung.png") {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
        }
        SystemProbe.open_read(path)
    }
}

/// Fix round 2, N1 part 1: without a known hash, only what can become a thumbnail is hashed. An
/// icon-only kind (a big video) and an image over 256 MiB answer `icon` at once with
/// `sha256: null`, never `pending` on a hash. (`.git` is a folder the index never enters; the
/// large files are made with `set_len` and removed at the end.)
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
#[allow(clippy::disallowed_methods)] // makes and removes the large files
async fn unindexed_icon_kinds_and_oversized_images_answer_icon_without_hashing() {
    const MIB: u64 = 1024 * 1024;
    let r = Rig::new(fast());
    let big = [
        (work(".git/clip.mp4"), 300 * MIB),
        (work(".git/huge.png"), 300 * MIB),
    ];
    for (rel, len) in &big {
        let file = rel.split('/').fold(r.ws.clone(), |p, c| p.join(c));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&file).unwrap();
        use std::io::Write as _;
        (&f).write_all(b"\x89PNG\r\n\x1a\n").unwrap();
        f.set_len(*len).unwrap();
    }
    r.put(&work(".git/notes.txt"), b"notes");
    let _open = r.open().await;
    let paths = [big[0].0.clone(), big[1].0.clone(), work(".git/notes.txt")];
    let items: Vec<ThumbItem> = paths.iter().map(|p| item(p)).collect();
    let states = r.thumbs.request(&items, 96).await;
    for state in &states {
        assert_icon(state);
        assert_eq!(state.sha256, None, "not hashed: {state:?}");
    }
    assert_eq!(states[0].media_kind, "video");
    assert_eq!(states[1].media_kind, "image");
    for (rel, _) in &big {
        std::fs::remove_file(rel.split('/').fold(r.ws.clone(), |p, c| p.join(c))).unwrap();
    }
    drop(_open);
    r.shutdown().await;
}

/// Fix round 2, N1 part 3: hashing has the decode deadline too. A hash whose read stalls
/// answers `icon` with `sha256: null` after the deadline, and the request is free; the next
/// request is answered meanwhile.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_hung_hash_answers_icon_after_its_deadline_and_frees_the_request() {
    let probe = Arc::new(HangingProbe::default());
    // Released however the test ends, so no thread stays hung.
    struct Release(Arc<HangingProbe>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let _release = Release(probe.clone());
    let r = Rig::with_probe(
        ThumbOptions {
            deadline: Duration::from_millis(300),
            ..fast()
        },
        true,
        probe.clone(),
    );
    let ok = picture(20, 20);
    r.put(&work(".git/hung.png"), &picture(20, 20));
    r.put(&work(".git/ok.png"), &ok);
    let _open = r.open().await;
    let states = tokio::time::timeout(
        Duration::from_secs(10),
        r.thumbs.request(&[item(&work(".git/hung.png"))], 96),
    )
    .await
    .expect("the hash's deadline frees the request");
    assert_icon(&states[0]);
    assert_eq!(states[0].sha256, None, "couldn't be hashed");
    let state = tokio::time::timeout(
        Duration::from_secs(10),
        r.thumbs.ensure(&ws_path(&work(".git/ok.png")), None, 96),
    )
    .await
    .expect("another item is answered meanwhile");
    assert_ready_at(&r.local(), &state, &sha256_bytes(&ok), 96);
    probe.release();
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn a_hung_decode_answers_icon_after_its_deadline_and_frees_its_permit() {
    let gated = Arc::new(Gated::default());
    let r = Rig::new(ThumbOptions {
        renderers: with_renderer(gated.clone()),
        permits: 1,
        deadline: Duration::from_millis(300),
        ..fast()
    });
    let hung = picture(40, 40);
    let next = picture_seeded(40, 40, 3);
    r.put(&work("hung.png"), &hung);
    r.put(&work("next.png"), &next);
    let _open = r.open().await;

    let state = tokio::time::timeout(
        DEADLINE,
        r.thumbs.ensure(&ws_path(&work("hung.png")), None, 96),
    )
    .await
    .expect("the deadline answers");
    assert_icon(&state);
    // The only permit is free again while the hung thread still runs.
    let state = tokio::time::timeout(
        DEADLINE,
        r.thumbs.ensure(&ws_path(&work("next.png")), None, 96),
    )
    .await
    .expect("the permit was released");
    assert_ready_at(&r.local(), &state, &sha256_bytes(&next), 96);
    // Once the hung render finishes, its result is discarded: nothing is filed for it.
    gated.release();
    let until = Instant::now() + DEADLINE;
    while !gated.hung_done.load(Ordering::SeqCst) {
        assert!(Instant::now() < until, "the hung render never finished");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(r.entries_of(&sha256_bytes(&hung)).is_empty());
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn inbox_and_unindexed_paths_get_their_hash() {
    let r = Rig::new(ThumbOptions {
        inline_wait: DEADLINE,
        ..ThumbOptions::default()
    });
    let _open = r.open().await;
    let inbox = picture(80, 40);
    let late = picture_seeded(80, 40, 1);
    let fresh = picture_seeded(80, 40, 2);
    let inbox_path = "inbox/in_0123456789abcdef/out.png";
    r.put(inbox_path, &inbox);
    r.put(&work("late.png"), &late);
    r.put(&work("fresh.png"), &fresh);

    // The inbox passes the hash from its inbox.json.
    let state = r
        .thumbs
        .ensure(&ws_path(inbox_path), Some(&sha256_bytes(&inbox)), 96)
        .await;
    assert_ready_at(&r.local(), &state, &sha256_bytes(&inbox), 96);
    // A path the caller knows no hash for is hashed on demand.
    let state = r.thumbs.ensure(&ws_path(&work("late.png")), None, 96).await;
    assert_ready_at(&r.local(), &state, &sha256_bytes(&late), 96);
    // So is one a request names before the index holds it.
    let states = r.thumbs.request(&[item(&work("fresh.png"))], 96).await;
    assert_ready_at(&r.local(), &states[0], &sha256_bytes(&fresh), 96);
    drop(_open);
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn clean_up_removes_hashes_the_index_no_longer_holds_and_keeps_inbox_ones() {
    let r = Rig::new(fast());
    let indexed = picture(30, 30);
    r.put(&work("kept.png"), &indexed);
    let open = r.open().await;
    let live = sha256_bytes(&indexed);
    assert!(open.index.file_hashes().await.unwrap().contains(&live));
    let gone = sha256_bytes(b"a file the index no longer holds");
    let inboxed = sha256_bytes(b"an inbox entry's file");
    let local = r.local();
    let old = KEEP_UNTOUCHED + DAY;
    let kept = plant(&local, &r.clock, &live, old);
    let removed = plant(&local, &r.clock, &gone, old);
    let inbox = plant(&local, &r.clock, &inboxed, old);
    // Something that isn't an entry is never touched.
    let other = local.join("thumbs").join(&gone[..2]).join("notes.txt");
    write_file(&other, b"not an entry");
    age(&other, &r.clock, old);
    let hashes = inboxed.clone();
    let source: LiveSource = Arc::new(move |_open| {
        let hashes = hashes.clone();
        Box::pin(async move { HashSet::from([hashes]) })
    });
    r.thumbs.add_live_source(source);
    drop(open);

    assert_eq!(r.thumbs.clean_up_pass().await, Some(1));
    assert!(kept.exists(), "the index holds its hash");
    assert!(inbox.exists(), "an inbox entry holds its hash");
    assert!(!removed.exists(), "nothing holds its hash");
    assert!(other.exists());
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn clean_up_skips_while_the_index_rebuilds_and_keeps_unverified_rows_hashes() {
    let r = Rig::new(fast());
    let local = r.local();
    let dead = plant(
        &local,
        &r.clock,
        &sha256_bytes(b"nothing holds this"),
        KEEP_UNTOUCHED + DAY,
    );
    // While the index rebuilds, nothing is removed.
    assert_eq!(r.thumbs.clean_up(&|_| false, false).await, 0);
    assert!(dead.exists());

    // A cold rebuild trusts the manifests' hashes: those rows stay unverified until checked,
    // and their hashes are live.
    let index =
        Arc::new(Index::open_in_memory(&ws_ref(&r.ws).path, Arc::new(SystemProbe)).unwrap());
    index.rebuild().await.unwrap();
    assert!(!index.rebuilding());
    let sealed = ws_path(SEALED);
    assert_eq!(index.is_verified(&sealed).await.unwrap(), Some(false));
    let recorded = index
        .files_of_path(&sealed)
        .await
        .unwrap()
        .and_then(|row| row.sha256)
        .expect("the manifest's hash");
    let hashes = index.file_hashes().await.unwrap();
    assert!(hashes.contains(&recorded));
    let unverified = plant(&local, &r.clock, &recorded, KEEP_UNTOUCHED + DAY);
    assert_eq!(
        r.thumbs.clean_up(&|sha| hashes.contains(sha), true).await,
        1
    );
    assert!(unverified.exists(), "an unverified row's hash is live");
    assert!(!dead.exists());
    index.close();
    r.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")] // spec: APP-038
async fn clean_up_keeps_entries_touched_within_7_days() {
    let r = Rig::new(fast());
    let local = r.local();
    let eight = plant(&local, &r.clock, &sha256_bytes(b"8"), 8 * DAY);
    let one = plant(&local, &r.clock, &sha256_bytes(b"1"), DAY);
    let almost = plant(
        &local,
        &r.clock,
        &sha256_bytes(b"7-"),
        KEEP_UNTOUCHED - Duration::from_secs(3600),
    );
    assert_eq!(r.thumbs.clean_up(&|_| false, true).await, 1);
    assert!(!eight.exists());
    assert!(one.exists());
    assert!(almost.exists());
    // The core's clock decides: a week later, both go.
    r.clock.advance(KEEP_UNTOUCHED);
    assert_eq!(r.thumbs.clean_up(&|_| false, true).await, 2);
    assert!(!one.exists() && !almost.exists());
    r.shutdown().await;
}

#[test] // spec: APP-038, APP-043
fn paths_outside_the_workspace_are_params_invalid() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("router");
    env.register_workspace(&ws, true);
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    let _core = start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        trusted: lil_core::identity::TrustedDirs {
            app_dirs: vec![lil_testkit::ensure_built().dir.clone()],
            test_app: true,
            ..base.trusted.clone()
        },
        ..base
    });
    let mut app =
        TestApp::start_with_capabilities(&env, Some(&ws), &[], lil_protocol::CAPABILITIES);
    let call = |app: &mut TestApp, params: Value| -> Value {
        let response = app.call("app/thumbs.request", params);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    };
    for bad in [
        r"..\outside.png",
        "../outside.png",
        "projects/../../outside.png",
        r"C:\outside.png",
        "/outside.png",
        r"\\server\share\x.png",
        "",
    ] {
        let result = call(&mut app, json!({"items": [{"path": bad}], "tier": 96}));
        assert_eq!(result["status"], "error", "{bad:?}: {result}");
        assert_eq!(
            result["error"]["code"], "params.invalid",
            "{bad:?}: {result}"
        );
    }
    for params in [
        json!({"items": [{"path": SEALED}], "tier": 100}),
        json!({"items": [], "tier": 96}),
        json!({"items": [{"path": SEALED}], "tier": 96, "scale": 8.0}),
        json!({"items": [{"path": SEALED}], "large_px": 9000}),
    ] {
        let result = call(&mut app, params.clone());
        assert_eq!(
            result["error"]["code"], "params.invalid",
            "{params}: {result}"
        );
    }
    // An item inside the workspace is answered.
    let result = call(&mut app, json!({"items": [{"path": SEALED}], "tier": 96}));
    assert_eq!(result["status"], "ok", "{result}");
    let thumb = &result["thumbs"][0];
    assert_eq!(thumb["path"], SEALED, "{result}");
    assert_eq!(thumb["media_kind"], "image", "{result}");
    assert!(
        matches!(thumb["state"].as_str(), Some("ready" | "pending" | "icon")),
        "{result}"
    );
}

#[test] // spec: APP-038, ARCH-112, ARCH-097
#[allow(clippy::disallowed_methods)] // makes the junction and the folders the test needs
fn writes_go_only_under_the_thumbs_folder() {
    let env = TestEnv::new();
    let local = env.root().join("local");
    let outside = env.root().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let denied = |r: std::io::Result<()>, what: &str| {
        let e = r.expect_err(what);
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{what}: {e}"
        );
    };
    // Allowed: below thumbs\ and picks\thumbs\, compared without regard to case.
    for ok in [
        local.join(r"thumbs\ab\x.png"),
        local.join(r"picks\thumbs\x.png"),
        local.join(r"THUMBS\cd\y.png"),
    ] {
        fsops::write_workspace_local_file(&local, &ok, b"png").unwrap();
        assert_eq!(std::fs::read(&ok).unwrap(), b"png");
    }
    // Content-keyed: a file already there is kept.
    fsops::write_workspace_local_file(&local, &local.join(r"thumbs\ab\x.png"), b"other").unwrap();
    assert_eq!(
        std::fs::read(local.join(r"thumbs\ab\x.png")).unwrap(),
        b"png"
    );
    // Refused before anything is written.
    for bad in [
        PathBuf::from(format!(r"{}\thumbs\..\x.png", local.display())),
        PathBuf::from(format!(r"{}\thumbs\ab\..\..\x.png", local.display())),
        PathBuf::from(format!(r"{}\thumbs\.\x.png", local.display())),
        local.join(r"other\x.png"),
        local.join(r"picks\x.png"),
        local.join("thumbs"),
        local.join("x.png"),
        outside.join(r"thumbs\x.png"),
        PathBuf::from(r"thumbs\ab\x.png"),
        local.join(r"thumbs\ab\x.png:stream"),
        local.join(r"thumbs\ab\CON.png"),
    ] {
        denied(
            fsops::write_workspace_local_file(&local, &bad, b"png"),
            &bad.display().to_string(),
        );
    }
    assert!(!local.join("x.png").exists() && !local.join("other").exists());
    assert!(!env.root().join("x.png").exists());
    // A link on the way is refused: nothing lands outside.
    lil_platform::fs::create_junction(&local.join(r"thumbs\ln"), &outside).unwrap();
    denied(
        fsops::write_workspace_local_file(&local, &local.join(r"thumbs\ln\x.png"), b"png"),
        "through a junction",
    );
    assert!(!outside.join("x.png").exists());
    // Removal: only below thumbs\, only files.
    denied(
        fsops::remove_workspace_local_file(&local, &local.join(r"picks\thumbs\x.png")),
        "a pick log thumbnail",
    );
    assert!(local.join(r"picks\thumbs\x.png").exists());
    denied(
        fsops::remove_workspace_local_file(&local, &local.join(r"thumbs\ab")),
        "a folder",
    );
    denied(
        fsops::remove_workspace_local_file(
            &local,
            &PathBuf::from(format!(r"{}\thumbs\..\picks\thumbs\x.png", local.display())),
        ),
        "a dot-dot path",
    );
    // A hard link planted in thumbs\ to a file outside: never touched (fix round 2, N2).
    let victim = outside.join("victim.bin");
    write_file(&victim, b"outside");
    let victim_time = std::fs::metadata(&victim).unwrap().modified().unwrap();
    std::fs::hard_link(&victim, local.join(r"thumbs\ab\h.png")).unwrap();
    // A touch (a cache hit): only below thumbs\, only a plain file.
    let when = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    fsops::touch_workspace_local_file(&local, &local.join(r"thumbs\ab\x.png"), when).unwrap();
    assert_eq!(
        std::fs::metadata(local.join(r"thumbs\ab\x.png"))
            .unwrap()
            .modified()
            .unwrap(),
        when
    );
    assert_eq!(
        std::fs::read(local.join(r"thumbs\ab\x.png")).unwrap(),
        b"png"
    );
    for (bad, what) in [
        (local.join(r"picks\thumbs\x.png"), "a pick log thumbnail"),
        (local.join(r"thumbs\ab"), "a folder"),
        (local.join(r"thumbs\ln"), "a junction"),
        (outside.join(r"thumbs\x.png"), "another folder"),
        (local.join(r"thumbs\ab\h.png"), "a hard link"),
    ] {
        denied(fsops::touch_workspace_local_file(&local, &bad, when), what);
    }
    assert_eq!(
        std::fs::metadata(&victim).unwrap().modified().unwrap(),
        victim_time,
        "the outside file's time is untouched"
    );
    std::fs::remove_file(local.join(r"thumbs\ab\h.png")).unwrap();
    fsops::remove_workspace_local_file(&local, &local.join(r"thumbs\ab\x.png")).unwrap();
    assert!(!local.join(r"thumbs\ab\x.png").exists());
    std::fs::remove_dir(local.join(r"thumbs\ln")).unwrap();
}

#[test] // spec: APP-038
fn has_renderer_agrees_with_the_registry() {
    fn every_kind() -> [MediaKind; 9] {
        // An exhaustive match: a new kind fails to compile here until it is listed.
        let listed = |k: MediaKind| match k {
            MediaKind::Image
            | MediaKind::Video
            | MediaKind::Audio
            | MediaKind::Model3d
            | MediaKind::Document
            | MediaKind::Text
            | MediaKind::Archive
            | MediaKind::Font
            | MediaKind::Other => k,
        };
        [
            listed(MediaKind::Image),
            MediaKind::Video,
            MediaKind::Audio,
            MediaKind::Model3d,
            MediaKind::Document,
            MediaKind::Text,
            MediaKind::Archive,
            MediaKind::Font,
            MediaKind::Other,
        ]
    }
    let registry = default_renderers();
    let labels: Vec<String> = registry
        .iter()
        .map(|r| format!("{}@{}", r.id(), r.version()))
        .collect();
    assert_eq!(labels, ["image@1", "icon@1"]);
    for kind in every_kind() {
        let own: Vec<&str> = registry
            .iter()
            .filter(|r| r.id() != ICON && r.renders(kind))
            .map(|r| r.id())
            .collect();
        let icon = registry.iter().any(|r| r.id() == ICON && r.renders(kind));
        assert_eq!(!own.is_empty(), kind.has_renderer(), "{kind}: {own:?}");
        assert_eq!(icon, !kind.has_renderer(), "{kind}");
    }
    // `renderable` is true exactly for the extensions `image@1` decodes.
    let known = [
        "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "psd", "exr", "hdr", "tga",
        "dds", "ktx2", "avif", "glb", "gltf", "fbx", "obj", "blend", "usd", "usdc", "usdz", "abc",
        "wav", "flac", "ogg", "mp3", "m4a", "aif", "aiff", "mp4", "mov", "webm", "mkv", "avi",
        "ttf", "otf", "woff", "woff2", "zip", "7z", "rar", "tar", "gz", "pdf", "doc", "docx",
        "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp", "txt", "md", "json", "yaml",
        "yml", "toml", "csv", "xml", "ini", "log", "exe", "bin",
    ];
    for ext in known {
        for name in [format!("a.{ext}"), format!("dir/A.{}", ext.to_uppercase())] {
            let decoded = DECODED_EXTENSIONS.contains(&ext);
            assert_eq!(renderable(&name), decoded, "{name}");
            if decoded {
                assert_eq!(media_kind_of(&name), MediaKind::Image, "{name}");
            }
        }
    }
}

/// Ruling 2 (pre-flight A67): the service resolves the default workspace on every call, so
/// after a switch the thumbnail comes from the new default workspace's cache.
#[tokio::test(flavor = "multi_thread")] // spec: APP-038, ARCH-097
async fn the_default_workspace_is_resolved_per_call() {
    let r = Rig::new(fast());
    let first = picture(50, 50);
    let path = work("same-name.png");
    r.put(&path, &first);
    let _open = r.open().await;
    let state = r.thumbs.ensure(&ws_path(&path), None, 96).await;
    assert_ready_at(&r.local(), &state, &sha256_bytes(&first), 96);

    let other = r.env.copy_fixture_workspace("other");
    let second = picture_seeded(50, 50, 77);
    write_file(
        &path.split('/').fold(other.clone(), |p, c| p.join(c)),
        &second,
    );
    r.env.register_workspace(&other, true);
    r.hub.refresh_default();
    let other_local = r.env.workspace_local_dir(&other);
    assert_ne!(other_local, r.local());
    let state = r.thumbs.ensure(&ws_path(&path), None, 96).await;
    assert_ready_at(&other_local, &state, &sha256_bytes(&second), 96);
    assert!(cache_files(&r.local(), &sha256_bytes(&second)).is_empty());
    drop(_open);
    r.shutdown().await;
}
