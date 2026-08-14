#[path = "capture.rs"]
mod capture;
#[path = "claude.rs"]
mod claude;
#[path = "clipboard.rs"]
mod clipboard;
#[path = "ocr.rs"]
mod ocr;
#[path = "openai.rs"]
mod openai;
#[path = "overlay.rs"]
mod overlay;
#[path = "provider.rs"]
mod provider;
#[path = "settings.rs"]
mod settings;
#[path = "ui_components.rs"]
mod ui_components;

use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use capture::CapturedScreen;
use eframe::egui::{
    self, Align, Color32, ColorImage, FontId, Layout, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Vec2, ViewportBuilder, ViewportCommand, ViewportId, WindowLevel,
};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager,
    hotkey::{Code, HotKey, Modifiers},
};
use ui_components::{
    action_card, configure_korean_font, configure_theme, friendly_error_message,
    model_display_name, reasoning_display_name,
};
use xcap::image::{RgbaImage, imageops};

const WINDOW_SIZE: Vec2 = Vec2::new(640.0, 520.0);
const WINDOW_MIN_SIZE: Vec2 = Vec2::new(560.0, 460.0);
const SCREEN_HOTKEY_LABEL: &str = "Ctrl + T";
const SHOW_CAPTURED_SELECTION_BACKGROUND: bool = !cfg!(target_os = "macos");
#[cfg(not(target_os = "macos"))]
const SELECTION_CLIPBOARD_HOTKEY_LABEL: &str = "Ctrl + B";
#[cfg(target_os = "macos")]
const SELECTION_CLIPBOARD_HOTKEY_LABEL: &str = "⌘ + B";
#[cfg(not(target_os = "macos"))]
const SELECT_ALL_LABEL: &str = "Ctrl+A";
#[cfg(target_os = "macos")]
const SELECT_ALL_LABEL: &str = "⌘A";
#[cfg(not(target_os = "macos"))]
const PASTE_LABEL: &str = "Ctrl+V";
#[cfg(target_os = "macos")]
const PASTE_LABEL: &str = "⌘V";
const TOAST_SIZE: Vec2 = Vec2::new(390.0, 82.0);
const TOAST_LIFETIME: Duration = Duration::from_secs(6);
const LIVE_SCAN_INTERVAL: Duration = Duration::from_millis(700);
const LIVE_RETRY_MAX: Duration = Duration::from_secs(10);
const LIVE_SLOW_RESPONSE_NOTICE: Duration = Duration::from_secs(55);
const HOTKEY_DEBOUNCE: Duration = Duration::from_millis(300);
const LIVE_BATCH_LIMIT: usize = 24;
const EMPTY_FRAME_GRACE: u8 = 3;
const OVERLAY_GAP: f32 = 6.0;
const OVERLAY_COMPACT_GAP: f32 = 2.0;
const OVERLAY_MIN_FONT_SCALE: f32 = 0.78;

pub(crate) fn run() -> eframe::Result {
    if let Some(input) = diagnostic_input() {
        if let Err(error) = run_diagnostic(input) {
            eprintln!("진단 실패: {error:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dog Translate")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(WINDOW_MIN_SIZE)
            .with_transparent(true),
        ..Default::default()
    };

    eframe::run_native(
        "Dog Translate",
        options,
        Box::new(|cc| Ok(Box::new(TranslatorApp::new(cc)))),
    )
}

enum DiagnosticInput {
    Image(std::path::PathBuf),
    KoreanText(String),
    Screen,
}

fn diagnostic_input() -> Option<DiagnosticInput> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let flag = args.next()?;
    if flag == "--diagnose-screen" {
        return Some(DiagnosticInput::Screen);
    }

    let value = args.next()?;
    if flag == "--translate-image" {
        Some(DiagnosticInput::Image(value.into()))
    } else if flag == "--translate-korean" {
        Some(DiagnosticInput::KoreanText(
            value.to_string_lossy().into_owned(),
        ))
    } else {
        None
    }
}

fn run_diagnostic(input: DiagnosticInput) -> anyhow::Result<()> {
    if matches!(input, DiagnosticInput::Screen) {
        return run_screen_diagnostic();
    }

    let (source, direction, source_label, target_label) = match input {
        DiagnosticInput::Image(path) => {
            let image = xcap::image::open(path)?.to_rgba8();
            (
                ocr::recognize_english(&image)?,
                openai::TranslationDirection::EnglishToKorean,
                "OCR",
                "KO",
            )
        }
        DiagnosticInput::KoreanText(text) => (
            text,
            openai::TranslationDirection::KoreanToEnglish,
            "KO",
            "EN",
        ),
        DiagnosticInput::Screen => unreachable!(),
    };
    let settings = settings::load();
    let requested_effort = settings.reasoning_efforts.get(&settings.model);
    let mut client = provider::LlmClient::connect(
        settings.provider,
        &settings.model,
        requested_effort.map(String::as_str),
        |status| eprintln!("{status}"),
    )?;
    eprintln!(
        "MODEL: {} · effort {} · 계정 모델 {}개",
        client.model(),
        client.reasoning_effort(),
        client.available_models().len()
    );
    for model in client.available_models() {
        eprintln!(
            "  - {} · {} · efforts {}",
            model.display_name,
            model.id,
            model.reasoning_efforts.join(", ")
        );
    }
    let result = client.translate(&source, direction)?;
    println!("{source_label}: {source}");
    println!("{target_label}: {}", result.text);
    println!("TIME: {:.2}s", result.wall_time.as_secs_f64());
    Ok(())
}

fn run_screen_diagnostic() -> anyhow::Result<()> {
    let permission_granted = capture::screen_capture_permission_granted();
    println!("PERMISSION: {permission_granted}");
    anyhow::ensure!(
        permission_granted,
        "macOS가 이 앱 번들의 화면 기록 권한을 허용하지 않은 상태로 인식합니다"
    );

    let captured = capture::capture_monitor_under_cursor()?;
    let image = &captured.image;
    let pixel_count = u64::from(image.width()) * u64::from(image.height());
    let sample_step = usize::try_from((pixel_count / 100_000).max(1)).unwrap_or(1);
    let mut sample_count = 0_u64;
    let mut luminance_sum = 0.0_f64;
    let mut luminance_min = 255.0_f64;
    let mut luminance_max = 0.0_f64;
    let mut alpha_min = u8::MAX;
    let mut alpha_max = u8::MIN;

    for pixel in image.pixels().step_by(sample_step) {
        let [red, green, blue, alpha] = pixel.0;
        let luminance =
            0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue);
        sample_count += 1;
        luminance_sum += luminance;
        luminance_min = luminance_min.min(luminance);
        luminance_max = luminance_max.max(luminance);
        alpha_min = alpha_min.min(alpha);
        alpha_max = alpha_max.max(alpha);
    }

    let luminance_mean = luminance_sum / sample_count.max(1) as f64;
    println!(
        "CAPTURE: {}x{} · origin ({}, {}) · scale {:.2}",
        image.width(),
        image.height(),
        captured.x,
        captured.y,
        captured.scale_factor
    );
    println!(
        "LUMINANCE: mean {luminance_mean:.1} · min {luminance_min:.1} · max {luminance_max:.1} · samples {sample_count}"
    );
    println!("ALPHA: min {alpha_min} · max {alpha_max}");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppPhase {
    Ready,
    Capturing,
    Selecting,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeAction {
    CancelSelection,
    StopLive,
}

fn escape_action_for_phase(phase: AppPhase) -> Option<EscapeAction> {
    match phase {
        AppPhase::Capturing | AppPhase::Selecting => Some(EscapeAction::CancelSelection),
        AppPhase::Live => Some(EscapeAction::StopLive),
        AppPhase::Ready => None,
    }
}

enum CaptureEvent {
    ManualCaptured {
        screen: CapturedScreen,
        session: u64,
    },
    LiveRegionCaptured {
        image: RgbaImage,
        session: u64,
    },
    ManualFailed {
        message: String,
        session: u64,
    },
    LiveFailed {
        message: String,
        session: u64,
    },
}

enum WorkerJob {
    Connect {
        provider: provider::ProviderKind,
        model: String,
        reasoning_effort: Option<String>,
    },
    TranslateSelection {
        cancelled: Arc<AtomicBool>,
    },
    TranslateLiveLines {
        targets: Vec<LiveTranslationTarget>,
        context_lines: Vec<String>,
        session: u64,
        cancelled: Arc<AtomicBool>,
    },
}

enum OcrJob {
    RecognizeLiveFrame { image: RgbaImage, session: u64 },
}

enum OcrEvent {
    Recognized {
        lines: Vec<ocr::RecognizedLine>,
        elapsed: Duration,
        session: u64,
    },
    Failed {
        message: String,
        session: u64,
    },
    Crashed,
}

#[derive(Debug, Clone)]
struct LiveTranslationTarget {
    key: String,
    source: String,
}

#[derive(Debug, Clone)]
struct PendingLiveTranslation {
    targets: Vec<LiveTranslationTarget>,
    context_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    ScreenOverlay,
    SelectionClipboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Working,
    Ready,
    Cancelled,
    Error,
}

struct ClipboardOperation {
    cancelled: Arc<AtomicBool>,
}

struct TranslationToast {
    kind: ToastKind,
    title: String,
    detail: String,
    dismiss_at: Option<Instant>,
}

impl TranslationToast {
    fn working() -> Self {
        let detail = if cfg!(target_os = "macos") {
            "번역 중 다시 ⌘B를 누르면 취소됩니다"
        } else {
            "번역 중 다시 Ctrl+B를 누르면 취소됩니다"
        };
        Self {
            kind: ToastKind::Working,
            title: "번역 중…".to_owned(),
            detail: detail.to_owned(),
            dismiss_at: None,
        }
    }

    fn complete(delivery: Delivery) -> Self {
        let (title, detail) = match delivery {
            Delivery::SelectionClipboard => {
                if cfg!(target_os = "macos") {
                    ("붙여넣기 준비 완료", "⌘V로 바로 붙여넣으세요")
                } else {
                    ("붙여넣기 준비 완료", "Ctrl+V로 바로 붙여넣으세요")
                }
            }
            Delivery::ScreenOverlay => ("번역 완료", "화면 번역이 완료됐어요"),
        };
        Self {
            kind: ToastKind::Ready,
            title: title.to_owned(),
            detail: detail.to_owned(),
            dismiss_at: Some(Instant::now() + TOAST_LIFETIME),
        }
    }

    fn error() -> Self {
        Self {
            kind: ToastKind::Error,
            title: "번역하지 못했어요".to_owned(),
            detail: "Dog Translate 창에서 오류 내용을 확인해 주세요".to_owned(),
            dismiss_at: Some(Instant::now() + TOAST_LIFETIME),
        }
    }

    fn cancelled() -> Self {
        Self {
            kind: ToastKind::Cancelled,
            title: "번역 취소됨".to_owned(),
            detail: "번역 결과를 클립보드에 저장하지 않았습니다".to_owned(),
            dismiss_at: Some(Instant::now() + Duration::from_secs(3)),
        }
    }
}

enum WorkerEvent {
    Status(String),
    Connected {
        plan_type: String,
        selected_model: String,
        selected_reasoning_effort: String,
        models: Vec<openai::ModelInfo>,
    },
    Complete {
        elapsed: Duration,
        delivery: Delivery,
    },
    LiveTranslations {
        translations: Vec<(String, String)>,
        elapsed: Duration,
        session: u64,
    },
    LiveFailed {
        message: String,
        session: u64,
    },
    Failed(String),
    Crashed,
}

#[derive(Debug, Clone)]
struct LiveRegion {
    monitor_x: i32,
    monitor_y: i32,
    capture_x: i32,
    capture_y: i32,
    window_x: f32,
    window_y: f32,
    width: u32,
    height: u32,
    display_width: u32,
    display_height: u32,
    scale_factor: f32,
}

#[derive(Debug, Clone)]
struct LiveOverlayLine {
    id: u64,
    source: String,
    translation: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    pending: bool,
}

struct TranslatorApp {
    phase: AppPhase,
    root_viewport_hidden: bool,
    capture_tx: Sender<CaptureEvent>,
    capture_rx: Receiver<CaptureEvent>,
    worker_tx: Sender<WorkerJob>,
    worker_rx: Receiver<WorkerEvent>,
    ocr_tx: Sender<OcrJob>,
    ocr_rx: Receiver<OcrEvent>,
    hotkey_manager: Option<GlobalHotKeyManager>,
    hotkey_rx: Receiver<GlobalHotKeyEvent>,
    screen_hotkey: HotKey,
    selection_clipboard_hotkey: HotKey,
    escape_hotkey: HotKey,
    escape_hotkey_registered: bool,
    hotkey_error: Option<String>,
    captured: Option<CapturedScreen>,
    texture: Option<TextureHandle>,
    drag_start: Option<Pos2>,
    drag_current: Option<Pos2>,
    status: String,
    timing: Option<String>,
    error: Option<String>,
    account_plan: Option<String>,
    selected_provider: provider::ProviderKind,
    selected_model: String,
    selected_reasoning_effort: String,
    reasoning_preferences: HashMap<String, String>,
    provider_models: HashMap<String, String>,
    overlay_layout: settings::OverlayLayoutMode,
    available_models: Vec<openai::ModelInfo>,
    advanced_settings_open: bool,
    main_position: Option<Pos2>,
    background_busy: bool,
    clipboard_operation: Option<ClipboardOperation>,
    translation_toast: Option<TranslationToast>,
    live_region: Option<LiveRegion>,
    live_lines: Vec<LiveOverlayLine>,
    live_in_flight: bool,
    live_translation_in_flight: bool,
    live_operation: Option<Arc<AtomicBool>>,
    live_pending_translation: Option<PendingLiveTranslation>,
    live_translation_cache: HashMap<String, String>,
    next_live_line_id: u64,
    live_started_at: Option<Instant>,
    live_next_scan: Instant,
    live_failure_streak: u32,
    live_empty_frames: u8,
    live_session: u64,
    capture_session: u64,
    last_screen_toggle: Option<Instant>,
}

impl TranslatorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_korean_font(&cc.egui_ctx);
        configure_theme(&cc.egui_ctx);

        let (capture_tx, capture_rx) = mpsc::channel();
        let (worker_tx, worker_rx) = spawn_worker();
        let (ocr_tx, ocr_rx) = spawn_ocr_worker();
        let settings = settings::load();
        let overlay_layout = settings.overlay_layout;
        let selected_provider = settings.provider;
        let provider_models = settings.provider_models;
        let selected_model = provider_models
            .get(selected_provider.settings_key())
            .cloned()
            .unwrap_or(settings.model);
        let reasoning_preferences = settings.reasoning_efforts;
        let selected_reasoning_effort = reasoning_preferences
            .get(&selected_model)
            .cloned()
            .unwrap_or_else(|| default_reasoning_for_model(&selected_model));

        let selection_modifier = if cfg!(target_os = "macos") {
            Modifiers::META
        } else {
            Modifiers::CONTROL
        };
        let screen_hotkey = HotKey::new(Some(Modifiers::CONTROL), Code::KeyT);
        let selection_clipboard_hotkey = HotKey::new(Some(selection_modifier), Code::KeyB);
        let escape_hotkey = HotKey::new(None, Code::Escape);
        let (hotkey_tx, hotkey_rx) = mpsc::channel();
        let hotkey_repaint = cc.egui_ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event| {
            let _ = hotkey_tx.send(event);
            hotkey_repaint.request_repaint();
        }));
        let (hotkey_manager, hotkey_error) = match GlobalHotKeyManager::new() {
            Ok(manager) => {
                let mut failures = Vec::new();
                for (name, hotkey) in [
                    ("실시간 화면 번역", screen_hotkey),
                    ("선택문 번역 후 붙여넣기", selection_clipboard_hotkey),
                ] {
                    if let Err(error) = manager.register(hotkey) {
                        failures.push(format!("{name}: {error}"));
                    }
                }
                let error = (!failures.is_empty())
                    .then(|| format!("단축키 등록 실패 · {}", failures.join(" · ")));
                (Some(manager), error)
            }
            Err(error) => (None, Some(format!("전역 단축키 초기화 실패: {error}"))),
        };

        let _ = worker_tx.send(WorkerJob::Connect {
            provider: selected_provider,
            model: selected_model.clone(),
            reasoning_effort: Some(selected_reasoning_effort.clone()),
        });

        Self {
            phase: AppPhase::Ready,
            root_viewport_hidden: false,
            capture_tx,
            capture_rx,
            worker_tx,
            worker_rx,
            ocr_tx,
            ocr_rx,
            hotkey_manager,
            hotkey_rx,
            screen_hotkey,
            selection_clipboard_hotkey,
            escape_hotkey,
            escape_hotkey_registered: false,
            hotkey_error,
            captured: None,
            texture: None,
            drag_start: None,
            drag_current: None,
            status: format!("{} 계정 연결 중…", selected_provider.display_name()),
            timing: None,
            error: None,
            account_plan: None,
            selected_provider,
            selected_model,
            selected_reasoning_effort,
            reasoning_preferences,
            provider_models,
            overlay_layout,
            available_models: Vec::new(),
            advanced_settings_open: false,
            main_position: None,
            background_busy: false,
            clipboard_operation: None,
            translation_toast: None,
            live_region: None,
            live_lines: Vec::new(),
            live_in_flight: false,
            live_translation_in_flight: false,
            live_operation: None,
            live_pending_translation: None,
            live_translation_cache: HashMap::new(),
            next_live_line_id: 1,
            live_started_at: None,
            live_next_scan: Instant::now(),
            live_failure_streak: 0,
            live_empty_frames: 0,
            live_session: 0,
            capture_session: 0,
            last_screen_toggle: None,
        }
    }

    fn begin_capture(&mut self, ctx: &egui::Context) {
        if matches!(self.phase, AppPhase::Capturing | AppPhase::Selecting) || self.background_busy {
            return;
        }

        if let Err(error) = capture::ensure_screen_capture_permission() {
            self.status = "화면 기록 권한이 필요합니다".to_owned();
            self.error = Some(format!("{error:#}"));
            return;
        }

        if self.main_position.is_none() {
            self.main_position =
                ctx.input(|input| input.viewport().outer_rect.map(|rect| rect.min));
        }
        self.phase = AppPhase::Capturing;
        self.set_escape_hotkey_registered(true);
        self.capture_session = self.capture_session.wrapping_add(1);
        let session = self.capture_session;
        self.error = None;
        self.status = "번역 창을 숨기고 화면을 캡처하는 중…".to_owned();
        self.hide_main_window(ctx);

        let sender = self.capture_tx.clone();
        let repaint = ctx.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(180));
            let event = match catch_unwind(AssertUnwindSafe(capture::capture_monitor_under_cursor))
            {
                Ok(Ok(screen)) => CaptureEvent::ManualCaptured { screen, session },
                Ok(Err(error)) => CaptureEvent::ManualFailed {
                    message: format!("{error:#}"),
                    session,
                },
                Err(_) => CaptureEvent::ManualFailed {
                    message: "화면 캡처 작업이 예기치 않게 중단되었습니다".to_owned(),
                    session,
                },
            };
            let _ = sender.send(event);
            repaint.request_repaint();
        });
    }

    fn toggle_live(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self
            .last_screen_toggle
            .is_some_and(|last| now.duration_since(last) < HOTKEY_DEBOUNCE)
        {
            return;
        }
        self.last_screen_toggle = Some(now);

        match self.phase {
            AppPhase::Live => self.stop_live(ctx, true),
            AppPhase::Capturing | AppPhase::Selecting => self.cancel_selection(ctx, true),
            AppPhase::Ready => self.begin_capture(ctx),
        }
    }

    fn handle_escape(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            return;
        }

        self.cancel_for_escape(ctx);
    }

    fn cancel_for_escape(&mut self, ctx: &egui::Context) {
        match escape_action_for_phase(self.phase) {
            Some(EscapeAction::CancelSelection) => self.cancel_selection(ctx, false),
            Some(EscapeAction::StopLive) => {
                self.stop_live(ctx, false);
                self.status = "영역 번역을 취소했습니다".to_owned();
            }
            None => {}
        }
    }

    fn set_escape_hotkey_registered(&mut self, registered: bool) {
        if self.escape_hotkey_registered == registered {
            return;
        }
        let Some(manager) = self.hotkey_manager.as_ref() else {
            return;
        };

        let result = if registered {
            manager.register(self.escape_hotkey)
        } else {
            manager.unregister(self.escape_hotkey)
        };
        match result {
            Ok(()) => self.escape_hotkey_registered = registered,
            Err(error) => {
                self.hotkey_error = Some(format!(
                    "Esc 취소 단축키 {} 실패: {error}",
                    if registered { "등록" } else { "해제" }
                ));
            }
        }
    }

    fn stop_live(&mut self, ctx: &egui::Context, show_main_window: bool) {
        self.set_escape_hotkey_registered(false);
        let had_in_flight_operation = self.live_translation_in_flight;
        self.cancel_active_live_operation();
        self.live_session = self.live_session.wrapping_add(1);
        self.live_lines.clear();
        self.live_pending_translation = None;
        self.live_translation_cache.clear();
        self.live_region = None;
        self.live_in_flight = false;
        self.live_translation_in_flight = false;
        self.live_started_at = None;
        self.live_failure_streak = 0;
        self.live_empty_frames = 0;
        self.phase = AppPhase::Ready;
        self.status = format!(
            "{} · 추론 {} 준비 완료",
            self.selected_model, self.selected_reasoning_effort
        );
        if had_in_flight_operation {
            self.restart_worker();
        }
        if show_main_window {
            self.restore_main_window(ctx);
        } else {
            self.hide_main_window(ctx);
        }
    }

    fn schedule_live_capture(&mut self, ctx: &egui::Context) {
        if self.phase != AppPhase::Live
            || self.live_in_flight
            || Instant::now() < self.live_next_scan
        {
            return;
        }
        let Some(region) = self.live_region.clone() else {
            return;
        };

        self.live_in_flight = true;
        let session = self.live_session;
        let sender = self.capture_tx.clone();
        let repaint = ctx.clone();
        thread::spawn(move || {
            let captured = catch_unwind(AssertUnwindSafe(|| {
                capture::capture_monitor_region(
                    region.monitor_x,
                    region.monitor_y,
                    region.capture_x,
                    region.capture_y,
                    region.width,
                    region.height,
                )
            }));
            let event = match captured {
                Ok(Ok(image)) => CaptureEvent::LiveRegionCaptured { image, session },
                Ok(Err(error)) => CaptureEvent::LiveFailed {
                    message: format!("{error:#}"),
                    session,
                },
                Err(_) => CaptureEvent::LiveFailed {
                    message: "실시간 화면 캡처 작업이 예기치 않게 중단되었습니다".to_owned(),
                    session,
                },
            };
            let _ = sender.send(event);
            repaint.request_repaint();
        });
    }

    fn receive_capture(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.capture_rx.try_recv() {
            match event {
                CaptureEvent::ManualCaptured { screen, session } => {
                    if session != self.capture_session || self.phase != AppPhase::Capturing {
                        continue;
                    }
                    let width = screen.image.width() as usize;
                    let height = screen.image.height() as usize;
                    let color_image =
                        ColorImage::from_rgba_unmultiplied([width, height], screen.image.as_raw());
                    self.texture = Some(ctx.load_texture(
                        "screen-capture",
                        color_image,
                        TextureOptions::LINEAR,
                    ));

                    let scale = screen.scale_factor;
                    self.root_viewport_hidden = false;
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    ctx.send_viewport_cmd(ViewportCommand::ContentProtected(false));
                    ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(false));
                    // On macOS, keep this as a transparent input overlay so the
                    // user selects against the real desktop. Repainting a full
                    // captured frame in an opaque NSWindow can be composited much
                    // darker than the source display.
                    ctx.send_viewport_cmd(ViewportCommand::Transparent(
                        !SHOW_CAPTURED_SELECTION_BACKGROUND,
                    ));
                    ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
                    ctx.send_viewport_cmd(ViewportCommand::Resizable(false));
                    ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
                    ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(
                        screen.x as f32 / scale,
                        screen.y as f32 / scale,
                    )));
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(
                        screen.image.width() as f32 / scale,
                        screen.image.height() as f32 / scale,
                    )));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);

                    self.captured = Some(screen);
                    self.drag_start = None;
                    self.drag_current = None;
                    self.phase = AppPhase::Selecting;
                    self.status = "번역할 영역을 드래그하세요".to_owned();
                }
                CaptureEvent::LiveRegionCaptured { image, session } => {
                    if session != self.live_session || self.phase != AppPhase::Live {
                        continue;
                    }
                    if self
                        .ocr_tx
                        .send(OcrJob::RecognizeLiveFrame { image, session })
                        .is_err()
                    {
                        self.live_in_flight = false;
                        self.error = Some("OCR 작업 스레드가 종료되었습니다".to_owned());
                        self.restart_ocr_worker();
                        self.schedule_live_retry("OCR 작업 스레드를 다시 시작했습니다".to_owned());
                    }
                }
                CaptureEvent::ManualFailed { message, session } => {
                    if session != self.capture_session || self.phase != AppPhase::Capturing {
                        continue;
                    }
                    self.restore_main_window(ctx);
                    self.set_escape_hotkey_registered(false);
                    self.phase = AppPhase::Ready;
                    self.error = Some(message);
                    self.status = "캡처 실패".to_owned();
                }
                CaptureEvent::LiveFailed { message, session } => {
                    if session != self.live_session {
                        continue;
                    }
                    self.schedule_live_retry(message);
                }
            }
        }
    }

    fn receive_ocr(&mut self) {
        loop {
            let event = match self.ocr_rx.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.live_in_flight = false;
                    self.restart_ocr_worker();
                    self.schedule_live_retry("OCR 작업을 자동으로 다시 시작했습니다".to_owned());
                    break;
                }
            };
            match event {
                OcrEvent::Recognized {
                    lines,
                    elapsed,
                    session,
                } => {
                    if session != self.live_session || self.phase != AppPhase::Live {
                        continue;
                    }
                    self.live_in_flight = false;
                    self.live_next_scan = Instant::now() + LIVE_SCAN_INTERVAL;
                    self.live_failure_streak = 0;
                    self.error = None;
                    self.timing = Some(format!("· OCR {:.2}초", elapsed.as_secs_f64()));

                    if lines.is_empty() {
                        self.live_empty_frames = self.live_empty_frames.saturating_add(1);
                        if self.live_empty_frames >= EMPTY_FRAME_GRACE {
                            self.live_lines.clear();
                        }
                    } else {
                        let mut reconciled = reconcile_live_lines(
                            &self.live_lines,
                            lines,
                            &self.live_translation_cache,
                            &mut self.next_live_line_id,
                        );
                        overlay::stabilize_lines(&self.live_lines, &mut reconciled);
                        self.live_empty_frames = 0;
                        self.live_lines = reconciled;
                        self.queue_live_translation();
                    }

                    if self.live_lines.is_empty() {
                        self.status = "선택 영역에서 영어 문장을 찾는 중…".to_owned();
                    } else if self.live_lines.iter().any(|line| line.pending) {
                        let pending = self.live_lines.iter().filter(|line| line.pending).count();
                        self.status = format!("새 문장 {pending}개 번역 중 · 화면 감지 계속 중");
                    } else {
                        self.status = "화면 변경 감시 중".to_owned();
                    }
                }
                OcrEvent::Failed { message, session } => {
                    if session != self.live_session || self.phase != AppPhase::Live {
                        continue;
                    }
                    self.live_in_flight = false;
                    self.schedule_live_retry(message);
                }
                OcrEvent::Crashed => {
                    self.live_in_flight = false;
                    self.restart_ocr_worker();
                    if self.phase == AppPhase::Live {
                        self.schedule_live_retry(
                            "OCR 작업을 자동으로 다시 시작했습니다".to_owned(),
                        );
                    }
                }
            }
        }
    }

    fn queue_live_translation(&mut self) {
        let mut targets = Vec::new();
        for line in &self.live_lines {
            let key = live_text_key(&line.source);
            if line.pending
                && !self.live_translation_cache.contains_key(&key)
                && !targets
                    .iter()
                    .any(|target: &LiveTranslationTarget| target.key == key)
            {
                targets.push(LiveTranslationTarget {
                    key,
                    source: line.source.clone(),
                });
            }
        }
        if targets.len() > LIVE_BATCH_LIMIT {
            targets.drain(..targets.len() - LIVE_BATCH_LIMIT);
        }
        if targets.is_empty() {
            return;
        }
        self.live_pending_translation = Some(PendingLiveTranslation {
            targets,
            context_lines: self
                .live_lines
                .iter()
                .map(|line| line.source.clone())
                .collect(),
        });
        self.dispatch_pending_live_translation();
    }

    fn dispatch_pending_live_translation(&mut self) {
        if self.live_translation_in_flight || self.phase != AppPhase::Live {
            return;
        }
        let Some(mut pending) = self.live_pending_translation.take() else {
            return;
        };
        pending
            .targets
            .retain(|target| !self.live_translation_cache.contains_key(&target.key));
        if pending.targets.is_empty() {
            return;
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        self.live_operation = Some(cancelled.clone());
        self.live_translation_in_flight = true;
        self.live_started_at = Some(Instant::now());
        if self
            .worker_tx
            .send(WorkerJob::TranslateLiveLines {
                targets: pending.targets,
                context_lines: pending.context_lines,
                session: self.live_session,
                cancelled,
            })
            .is_err()
        {
            self.live_translation_in_flight = false;
            self.live_operation = None;
            self.live_started_at = None;
            self.handle_worker_loss();
        }
    }

    fn receive_worker(&mut self) {
        loop {
            let event = match self.worker_rx.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.handle_worker_loss();
                    break;
                }
            };
            match event {
                WorkerEvent::Status(status) => self.status = status,
                WorkerEvent::Connected {
                    plan_type,
                    selected_model,
                    selected_reasoning_effort,
                    models,
                } => {
                    self.selected_model = selected_model;
                    self.selected_reasoning_effort = selected_reasoning_effort;
                    self.available_models = models;
                    self.provider_models.insert(
                        self.selected_provider.settings_key().to_owned(),
                        self.selected_model.clone(),
                    );
                    self.reasoning_preferences.insert(
                        self.selected_model.clone(),
                        self.selected_reasoning_effort.clone(),
                    );
                    let _ = self.save_settings();
                    if self.phase != AppPhase::Live {
                        self.status = format!(
                            "{} 연결 완료 · 번역 준비됨",
                            self.selected_provider.display_name()
                        );
                    }
                    self.account_plan = Some(plan_type);
                    self.timing = None;
                }
                WorkerEvent::Complete { elapsed, delivery } => {
                    self.timing = Some(format_metrics(elapsed));
                    self.error = None;
                    self.background_busy = false;
                    if delivery != Delivery::ScreenOverlay {
                        self.clipboard_operation = None;
                        self.translation_toast = Some(TranslationToast::complete(delivery));
                    }
                    match delivery {
                        Delivery::ScreenOverlay => {
                            self.status = "화면 번역 완료".to_owned();
                            self.phase = AppPhase::Ready;
                        }
                        Delivery::SelectionClipboard => {
                            self.status =
                                format!("영어 번역 준비 완료 · {PASTE_LABEL}로 붙여넣으세요");
                            self.phase = AppPhase::Ready;
                        }
                    }
                }
                WorkerEvent::LiveTranslations {
                    translations,
                    elapsed,
                    session,
                } => {
                    if session != self.live_session || self.phase != AppPhase::Live {
                        continue;
                    }
                    let translated_count = translations.len();
                    for (key, translation) in translations {
                        if !translation.trim().is_empty() {
                            self.live_translation_cache.insert(key, translation);
                        }
                    }
                    if self.live_translation_cache.len() > 4_096 {
                        let active_keys = self
                            .live_lines
                            .iter()
                            .map(|line| live_text_key(&line.source))
                            .collect::<Vec<_>>();
                        self.live_translation_cache
                            .retain(|key, _| active_keys.contains(key));
                    }
                    for line in &mut self.live_lines {
                        if let Some(translation) = self
                            .live_translation_cache
                            .get(&live_text_key(&line.source))
                        {
                            line.translation.clone_from(translation);
                            line.pending = false;
                        }
                    }
                    self.live_translation_in_flight = false;
                    self.live_operation = None;
                    self.live_started_at = None;
                    self.error = None;
                    self.timing = Some(format_metrics(elapsed));
                    self.status = format!("새 문장 {translated_count}개 번역 완료 · 변경 감시 중");
                    self.dispatch_pending_live_translation();
                }
                WorkerEvent::LiveFailed { message, session } => {
                    if session != self.live_session || self.phase != AppPhase::Live {
                        continue;
                    }
                    self.cancel_active_live_operation();
                    self.live_translation_in_flight = false;
                    self.live_started_at = None;
                    self.schedule_live_retry(message);
                }
                WorkerEvent::Failed(message) => {
                    if self.phase == AppPhase::Live {
                        self.schedule_live_retry(message);
                    } else {
                        if self.background_busy {
                            self.translation_toast = Some(TranslationToast::error());
                        }
                        self.cancel_active_clipboard_operation();
                        self.error = Some(message);
                        self.status = "처리 실패".to_owned();
                        self.background_busy = false;
                        self.phase = AppPhase::Ready;
                    }
                }
                WorkerEvent::Crashed => self.handle_worker_loss(),
            }
        }
    }

    fn schedule_live_retry(&mut self, message: String) {
        self.live_in_flight = false;
        self.live_failure_streak = self.live_failure_streak.saturating_add(1);
        let delay = live_retry_delay(self.live_failure_streak);
        self.live_next_scan = Instant::now() + delay;
        self.error = Some(message);
        self.status = format!("일시 오류 · {:.1}초 후 자동 재시도", delay.as_secs_f32());
    }

    fn restart_worker(&mut self) {
        let (worker_tx, worker_rx) = spawn_worker();
        self.worker_tx = worker_tx;
        self.worker_rx = worker_rx;
        self.account_plan = None;
        let _ = self.worker_tx.send(WorkerJob::Connect {
            provider: self.selected_provider,
            model: self.selected_model.clone(),
            reasoning_effort: Some(self.selected_reasoning_effort.clone()),
        });
    }

    fn restart_ocr_worker(&mut self) {
        let (ocr_tx, ocr_rx) = spawn_ocr_worker();
        self.ocr_tx = ocr_tx;
        self.ocr_rx = ocr_rx;
    }

    fn change_model(&mut self, model: String) {
        if model == self.selected_model || self.phase != AppPhase::Ready || self.background_busy {
            return;
        }
        self.selected_model = model;
        self.provider_models.insert(
            self.selected_provider.settings_key().to_owned(),
            self.selected_model.clone(),
        );
        self.selected_reasoning_effort = self.preferred_reasoning_for(&self.selected_model);
        self.reasoning_preferences.insert(
            self.selected_model.clone(),
            self.selected_reasoning_effort.clone(),
        );
        self.error = self.save_settings().err();
        self.status = "모델 설정을 적용하는 중…".to_owned();
        self.timing = None;
        self.restart_worker();
    }

    fn change_provider(&mut self, provider: provider::ProviderKind) {
        if provider == self.selected_provider
            || self.phase != AppPhase::Ready
            || self.background_busy
        {
            return;
        }
        self.cancel_active_live_operation();
        self.selected_provider = provider;
        self.selected_model = self
            .provider_models
            .get(provider.settings_key())
            .cloned()
            .unwrap_or_else(|| provider::default_model(provider).to_owned());
        self.selected_reasoning_effort = self.preferred_reasoning_for(&self.selected_model);
        self.available_models.clear();
        self.account_plan = None;
        self.error = self.save_settings().err();
        self.status = format!("{} 계정 연결 중…", provider.display_name());
        self.timing = None;
        self.restart_worker();
    }

    fn change_reasoning_effort(&mut self, effort: String) {
        let supported = self
            .available_models
            .iter()
            .find(|model| model.id == self.selected_model)
            .is_some_and(|model| model.reasoning_efforts.contains(&effort));
        if !supported
            || effort == self.selected_reasoning_effort
            || self.phase != AppPhase::Ready
            || self.background_busy
        {
            return;
        }
        self.selected_reasoning_effort = effort;
        self.reasoning_preferences.insert(
            self.selected_model.clone(),
            self.selected_reasoning_effort.clone(),
        );
        self.error = self.save_settings().err();
        self.status = "속도·정확도 설정을 적용하는 중…".to_owned();
        self.timing = None;
        self.restart_worker();
    }

    fn preferred_reasoning_for(&self, model_id: &str) -> String {
        let Some(model) = self
            .available_models
            .iter()
            .find(|model| model.id == model_id)
        else {
            return self
                .reasoning_preferences
                .get(model_id)
                .cloned()
                .unwrap_or_else(|| default_reasoning_for_model(model_id));
        };
        self.reasoning_preferences
            .get(model_id)
            .filter(|effort| model.reasoning_efforts.contains(effort))
            .cloned()
            .or_else(|| model.reasoning_efforts.first().cloned())
            .unwrap_or_else(|| default_reasoning_for_model(model_id))
    }

    fn save_settings(&self) -> Result<(), String> {
        settings::save(&settings::AppSettings {
            provider: self.selected_provider,
            model: self.selected_model.clone(),
            provider_models: self.provider_models.clone(),
            reasoning_efforts: self.reasoning_preferences.clone(),
            overlay_layout: self.overlay_layout,
        })
    }

    fn change_overlay_layout(&mut self, layout: settings::OverlayLayoutMode) {
        if layout == self.overlay_layout {
            return;
        }
        self.overlay_layout = layout;
        self.error = self.save_settings().err();
        self.status = format!("화면 배치: {}", overlay_layout_display_name(layout));
    }

    fn handle_worker_loss(&mut self) {
        self.cancel_active_clipboard_operation();
        self.cancel_active_live_operation();
        self.live_translation_in_flight = false;
        self.live_started_at = None;
        self.restart_worker();
        if self.phase == AppPhase::Live {
            self.live_session = self.live_session.wrapping_add(1);
            self.schedule_live_retry("번역 작업을 자동으로 다시 시작했습니다".to_owned());
        } else {
            self.background_busy = false;
            self.phase = AppPhase::Ready;
            self.error = Some("번역 작업을 자동으로 다시 시작했습니다".to_owned());
            self.status = "번역 연결 복구 중…".to_owned();
        }
    }

    fn update_live_wait_status(&mut self) {
        if self.phase != AppPhase::Live || !self.live_translation_in_flight {
            return;
        }
        if !self
            .live_started_at
            .is_some_and(|started| started.elapsed() >= LIVE_SLOW_RESPONSE_NOTICE)
        {
            return;
        }

        self.error = None;
        self.status = format!("응답이 늦지만 계속 기다리는 중 · {SCREEN_HOTKEY_LABEL}로 종료");
    }

    fn handle_hotkey(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.hotkey_rx.try_recv() {
            if event.state != global_hotkey::HotKeyState::Pressed {
                continue;
            }
            if event.id == self.escape_hotkey.id() {
                self.cancel_for_escape(ctx);
            } else if event.id == self.screen_hotkey.id() {
                self.toggle_live(ctx);
            } else if event.id == self.selection_clipboard_hotkey.id()
                && !self.cancel_selection_translation()
            {
                self.begin_selection_translation();
            }
        }
    }

    fn begin_selection_translation(&mut self) {
        if self.background_busy
            || matches!(
                self.phase,
                AppPhase::Capturing | AppPhase::Selecting | AppPhase::Live
            )
        {
            return;
        }
        self.background_busy = true;
        self.error = None;
        self.timing = None;
        self.status = "선택한 한국어를 읽는 중…".to_owned();
        self.translation_toast = Some(TranslationToast::working());
        let cancelled = Arc::new(AtomicBool::new(false));
        self.clipboard_operation = Some(ClipboardOperation {
            cancelled: cancelled.clone(),
        });
        if self
            .worker_tx
            .send(WorkerJob::TranslateSelection { cancelled })
            .is_err()
        {
            self.handle_worker_loss();
        }
    }

    fn cancel_selection_translation(&mut self) -> bool {
        let Some(operation) = self.clipboard_operation.take() else {
            return false;
        };

        operation.cancelled.store(true, Ordering::Release);
        self.background_busy = false;
        self.error = None;
        self.timing = None;
        self.phase = AppPhase::Ready;
        self.status = "번역을 취소했습니다 · 새 작업을 시작할 수 있습니다".to_owned();
        self.translation_toast = Some(TranslationToast::cancelled());

        // A fresh worker lets the next request start immediately. The old worker
        // still observes its cancellation token before it can touch the clipboard.
        self.restart_worker();
        true
    }

    fn cancel_active_clipboard_operation(&mut self) {
        if let Some(operation) = self.clipboard_operation.take() {
            operation.cancelled.store(true, Ordering::Release);
        }
    }

    fn cancel_active_live_operation(&mut self) {
        if let Some(cancelled) = self.live_operation.take() {
            cancelled.store(true, Ordering::Release);
        }
    }

    fn update_translation_toast(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self
            .translation_toast
            .as_ref()
            .and_then(|toast| toast.dismiss_at)
            .is_some_and(|dismiss_at| now >= dismiss_at)
        {
            self.translation_toast = None;
            return;
        }
        if self.translation_toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn show_translation_toast(&self, ctx: &egui::Context) {
        let Some(toast) = &self.translation_toast else {
            return;
        };
        let title = toast.title.clone();
        let detail = toast.detail.clone();
        let kind = toast.kind;
        let monitor_size = ctx.input(|input| {
            input
                .viewport()
                .monitor_size
                .unwrap_or(Vec2::new(1920.0, 1080.0))
        });
        let position = Pos2::new(((monitor_size.x - TOAST_SIZE.x) * 0.5).max(12.0), 24.0);
        let builder = ViewportBuilder::default()
            .with_title("Dog Translate 알림")
            .with_inner_size(TOAST_SIZE)
            .with_position(position)
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_has_shadow(false)
            .with_active(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_mouse_passthrough(true);

        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("translation-status-toast"),
            builder,
            move |ui, _class| {
                let rect = ui.max_rect().shrink(5.0);
                let accent = match kind {
                    ToastKind::Working => Color32::from_rgb(74, 190, 242),
                    ToastKind::Ready => Color32::from_rgb(69, 214, 166),
                    ToastKind::Cancelled => Color32::from_rgb(245, 181, 72),
                    ToastKind::Error => Color32::from_rgb(255, 115, 126),
                };
                ui.painter().rect(
                    rect,
                    14.0,
                    Color32::from_rgba_unmultiplied(15, 22, 32, 246),
                    Stroke::new(1.0, accent.gamma_multiply(0.65)),
                    StrokeKind::Inside,
                );
                ui.painter().circle_filled(
                    Pos2::new(rect.min.x + 24.0, rect.center().y),
                    6.0,
                    accent,
                );
                ui.painter().text(
                    rect.min + Vec2::new(43.0, 15.0),
                    egui::Align2::LEFT_TOP,
                    &title,
                    FontId::proportional(16.0),
                    Color32::from_rgb(238, 245, 253),
                );
                ui.painter().text(
                    rect.min + Vec2::new(43.0, 43.0),
                    egui::Align2::LEFT_TOP,
                    &detail,
                    FontId::proportional(12.0),
                    Color32::from_rgb(148, 163, 184),
                );
                if kind == ToastKind::Working {
                    let travel = (rect.width() - 24.0).max(1.0);
                    let progress = ((ui.input(|input| input.time) * 0.65).fract() as f32) * travel;
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            Pos2::new(rect.min.x + 12.0 + progress, rect.max.y - 4.0),
                            Vec2::new(34.0_f32.min(travel - progress), 2.0),
                        ),
                        1.0,
                        accent,
                    );
                }
            },
        );
    }

    fn selection_ui(&mut self, root_ui: &mut egui::Ui) {
        let ctx = root_ui.ctx().clone();
        let Some(texture) = self.texture.as_ref() else {
            return;
        };

        let mut finished = false;
        let mut toolbar_position = Pos2::new(18.0, 18.0);
        let mut toolbar_bounds = Rect::from_min_size(toolbar_position, Vec2::new(570.0, 82.0));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(root_ui, |ui| {
                let rect = ui.max_rect();
                toolbar_position = rect.min + Vec2::new(18.0, 18.0);
                toolbar_bounds = Rect::from_min_size(toolbar_position, Vec2::new(570.0, 82.0));
                let response = ui.interact(rect, ui.id().with("selection-area"), Sense::drag());
                if SHOW_CAPTURED_SELECTION_BACKGROUND {
                    ui.painter().image(
                        texture.id(),
                        rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                if response.drag_started() {
                    let pointer = response.interact_pointer_pos();
                    if pointer.is_some_and(|position| !toolbar_bounds.contains(position)) {
                        self.drag_start = pointer;
                        self.drag_current = self.drag_start;
                    }
                }
                if response.dragged() && self.drag_start.is_some() {
                    self.drag_current = response.interact_pointer_pos();
                }
                if response.drag_stopped() && self.drag_start.is_some() {
                    self.drag_current = response.interact_pointer_pos();
                    finished = true;
                }

                if let (Some(start), Some(current)) = (self.drag_start, self.drag_current) {
                    let selection = Rect::from_two_pos(start, current).intersect(rect);
                    if SHOW_CAPTURED_SELECTION_BACKGROUND {
                        let uv = Rect::from_min_max(
                            Pos2::new(
                                (selection.min.x - rect.min.x) / rect.width(),
                                (selection.min.y - rect.min.y) / rect.height(),
                            ),
                            Pos2::new(
                                (selection.max.x - rect.min.x) / rect.width(),
                                (selection.max.y - rect.min.y) / rect.height(),
                            ),
                        );
                        ui.painter()
                            .image(texture.id(), selection, uv, Color32::WHITE);
                    }
                    ui.painter().rect_stroke(
                        selection,
                        0.0,
                        Stroke::new(2.0, Color32::from_rgb(55, 200, 255)),
                        StrokeKind::Inside,
                    );
                }
            });

        let mut layout_choice = self.overlay_layout;
        egui::Area::new(egui::Id::new("selection-layout-toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(toolbar_position)
            .show(&ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(10, 15, 23, 242))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(55, 190, 237)))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("번역 단위")
                                    .size(13.0)
                                    .strong()
                                    .color(Color32::WHITE),
                            );
                            ui.selectable_value(
                                &mut layout_choice,
                                settings::OverlayLayoutMode::Reader,
                                "한 줄",
                            );
                            ui.selectable_value(
                                &mut layout_choice,
                                settings::OverlayLayoutMode::Paragraph,
                                "문단",
                            );
                            ui.selectable_value(
                                &mut layout_choice,
                                settings::OverlayLayoutMode::Source,
                                "원문 위치",
                            );
                        });
                        ui.add_space(3.0);
                        ui.label(
                            egui::RichText::new("단위를 고른 뒤 번역할 영역을 드래그 · Esc 취소")
                                .size(12.0)
                                .color(Color32::from_rgb(174, 192, 214)),
                        );
                    });
            });
        if layout_choice != self.overlay_layout {
            self.change_overlay_layout(layout_choice);
            self.status = format!(
                "{} 선택 · 번역할 영역을 드래그하세요",
                overlay_layout_display_name(layout_choice)
            );
        }

        if finished {
            self.finish_selection(&ctx);
        }
    }

    fn finish_selection(&mut self, ctx: &egui::Context) {
        let (Some(start), Some(end), Some(screen)) =
            (self.drag_start, self.drag_current, self.captured.as_ref())
        else {
            return;
        };

        let viewport = ctx.content_rect();
        let selection = Rect::from_two_pos(start, end).intersect(viewport);
        if selection.width() < 8.0 || selection.height() < 8.0 {
            self.error = Some("선택 영역이 너무 작습니다. 다시 드래그하세요.".to_owned());
            self.drag_start = None;
            self.drag_current = None;
            return;
        }

        let image_width = screen.image.width();
        let image_height = screen.image.height();
        let x = (((selection.min.x - viewport.min.x) / viewport.width()) * image_width as f32)
            .floor()
            .clamp(0.0, (image_width - 1) as f32) as u32;
        let y = (((selection.min.y - viewport.min.y) / viewport.height()) * image_height as f32)
            .floor()
            .clamp(0.0, (image_height - 1) as f32) as u32;
        let right = (((selection.max.x - viewport.min.x) / viewport.width()) * image_width as f32)
            .ceil()
            .clamp((x + 1) as f32, image_width as f32) as u32;
        let bottom = (((selection.max.y - viewport.min.y) / viewport.height())
            * image_height as f32)
            .ceil()
            .clamp((y + 1) as f32, image_height as f32) as u32;

        let crop = imageops::crop_imm(&screen.image, x, y, right - x, bottom - y).to_image();
        let scale = screen.scale_factor;
        let region = LiveRegion {
            monitor_x: screen.x,
            monitor_y: screen.y,
            capture_x: x as i32,
            capture_y: y as i32,
            window_x: screen.x as f32 / scale,
            window_y: screen.y as f32 / scale,
            width: right - x,
            height: bottom - y,
            display_width: screen.image.width(),
            display_height: screen.image.height(),
            scale_factor: scale,
        };

        self.live_session = self.live_session.wrapping_add(1);
        let session = self.live_session;
        self.phase = AppPhase::Live;
        self.status = "선택 영역의 영어 위치를 찾는 중…".to_owned();
        self.error = None;
        self.timing = None;
        self.live_lines.clear();
        self.live_in_flight = true;
        self.live_translation_in_flight = false;
        self.live_operation = None;
        self.live_pending_translation = None;
        self.live_translation_cache.clear();
        self.next_live_line_id = 1;
        self.live_started_at = None;
        self.live_next_scan = Instant::now() + LIVE_SCAN_INTERVAL;
        self.live_failure_streak = 0;
        self.live_empty_frames = 0;
        self.live_region = Some(region.clone());
        self.captured = None;
        self.texture = None;
        self.drag_start = None;
        self.drag_current = None;
        self.show_live_window(ctx, &region);

        if self
            .ocr_tx
            .send(OcrJob::RecognizeLiveFrame {
                image: crop,
                session,
            })
            .is_err()
        {
            self.restart_ocr_worker();
            self.schedule_live_retry("OCR 작업 연결을 다시 시작했습니다".to_owned());
        }
    }

    fn cancel_selection(&mut self, ctx: &egui::Context, show_main_window: bool) {
        self.set_escape_hotkey_registered(false);
        self.capture_session = self.capture_session.wrapping_add(1);
        self.captured = None;
        self.texture = None;
        self.drag_start = None;
        self.drag_current = None;
        self.phase = AppPhase::Ready;
        self.status = "영역 선택을 취소했습니다".to_owned();
        if show_main_window {
            self.restore_main_window(ctx);
        } else {
            self.hide_main_window(ctx);
        }
    }

    fn configure_main_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(ViewportCommand::ContentProtected(false));
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(false));
        ctx.send_viewport_cmd(ViewportCommand::Transparent(false));
        ctx.send_viewport_cmd(ViewportCommand::Decorations(true));
        ctx.send_viewport_cmd(ViewportCommand::Resizable(true));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(WINDOW_SIZE));
        if let Some(position) = self.main_position {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(position));
        }
    }

    fn restore_main_window(&mut self, ctx: &egui::Context) {
        self.root_viewport_hidden = false;
        self.configure_main_window(ctx);
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn hide_main_window(&mut self, ctx: &egui::Context) {
        // Viewport commands take effect after this frame. Suppress root UI
        // immediately so cancelling with Esc cannot paint the main window for
        // one frame before AppKit hides it.
        self.root_viewport_hidden = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn show_live_window(&mut self, ctx: &egui::Context, region: &LiveRegion) {
        self.root_viewport_hidden = false;
        let scale = region.scale_factor;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(ViewportCommand::Transparent(true));
        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(ViewportCommand::Resizable(false));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(
            region.window_x,
            region.window_y,
        )));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(
            region.display_width as f32 / scale,
            region.display_height as f32 / scale,
        )));
        ctx.send_viewport_cmd(ViewportCommand::ContentProtected(true));
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    }

    fn live_ui(&mut self, root_ui: &mut egui::Ui) {
        let Some(region) = self.live_region.clone() else {
            return;
        };
        let scale = region.scale_factor;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(root_ui, |ui| {
                let painter = ui.painter();
                let viewport = ui.max_rect();
                let bounds = viewport.shrink(6.0);
                let selected_rect = Rect::from_min_size(
                    viewport.min
                        + Vec2::new(
                            region.capture_x as f32 / scale,
                            region.capture_y as f32 / scale,
                        ),
                    Vec2::new(region.width as f32 / scale, region.height as f32 / scale),
                )
                .intersect(viewport);
                let column_bounds = overlay_column_bounds(selected_rect, bounds);
                let overlay_painter = painter.with_clip_rect(column_bounds);

                painter.rect_stroke(
                    selected_rect,
                    4.0,
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(59, 196, 239, 105)),
                    StrokeKind::Inside,
                );

                let mut ordered_lines: Vec<_> = self.live_lines.iter().collect();
                ordered_lines.sort_by(|left, right| {
                    left.y
                        .total_cmp(&right.y)
                        .then_with(|| left.x.total_cmp(&right.x))
                });
                let source_rects = ordered_lines
                    .iter()
                    .map(|line| {
                        Rect::from_min_size(
                            Pos2::new(
                                selected_rect.min.x + line.x / scale,
                                selected_rect.min.y + line.y / scale,
                            ),
                            Vec2::new(line.width / scale, line.height / scale),
                        )
                    })
                    .collect::<Vec<_>>();

                let layout_mode = self.overlay_layout;
                let display_ranges = if layout_mode == settings::OverlayLayoutMode::Paragraph {
                    detect_paragraph_ranges(&ordered_lines, &source_rects)
                } else {
                    (0..ordered_lines.len())
                        .map(|index| (index, index + 1))
                        .collect()
                };
                let display_rects = display_ranges
                    .iter()
                    .map(|&(start, end)| union_rects(&source_rects[start..end]))
                    .collect::<Vec<_>>();
                let build_prepared = |font_scale: f32| {
                    display_ranges
                        .iter()
                        .zip(display_rects.iter().copied())
                        .map(|(&(start, end), source_rect)| {
                            let lines = &ordered_lines[start..end];
                            let block_width = overlay_block_width(
                                layout_mode,
                                source_rect.width(),
                                column_bounds.width(),
                            );
                            let wrap_width = overlay_wrap_width(block_width);
                            let pending = lines.iter().any(|line| line.pending);
                            let display_text =
                                if layout_mode == settings::OverlayLayoutMode::Paragraph && pending
                                {
                                    let completed = lines
                                        .iter()
                                        .filter(|line| !line.pending)
                                        .map(|line| format_overlay_translation(&line.translation))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    if completed.is_empty() {
                                        "문단 번역 중…".to_owned()
                                    } else {
                                        format!("{completed} · 번역 중…")
                                    }
                                } else {
                                    lines
                                        .iter()
                                        .map(|line| {
                                            if line.pending {
                                                "번역 중…".to_owned()
                                            } else {
                                                format_overlay_translation(&line.translation)
                                            }
                                        })
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                };
                            let reserve_text = lines
                                .iter()
                                .map(|line| format_overlay_translation(&line.source))
                                .collect::<Vec<_>>()
                                .join(" ");
                            let character_count = display_text
                                .chars()
                                .count()
                                .max(reserve_text.chars().count())
                                as f32;
                            let source_line_height = source_rects[start..end]
                                .iter()
                                .map(Rect::height)
                                .fold(0.0, f32::max);
                            let mut font_size =
                                (source_line_height * 0.9).clamp(13.0, 18.0) * font_scale;
                            if character_count > 100.0 {
                                font_size = (font_size - 1.0).max(11.0);
                            }
                            let text_color = if pending {
                                Color32::from_rgb(172, 224, 246)
                            } else {
                                Color32::WHITE
                            };
                            let font = FontId::proportional(font_size);
                            let galley = overlay_painter.layout(
                                display_text,
                                font.clone(),
                                text_color,
                                wrap_width,
                            );
                            let reserve_galley =
                                overlay_painter.layout(reserve_text, font, text_color, wrap_width);
                            let reserved_size = Vec2::new(
                                galley.size().x.max(reserve_galley.size().x),
                                galley.size().y.max(reserve_galley.size().y),
                            );
                            let size = overlay_block_size(
                                reserved_size,
                                block_width,
                                layout_mode == settings::OverlayLayoutMode::Source,
                            );
                            (galley, text_color, pending, size)
                        })
                        .collect::<Vec<_>>()
                };

                let mut prepared = build_prepared(1.0);
                let mut requested_sizes = prepared
                    .iter()
                    .map(|(_, _, _, size)| *size)
                    .collect::<Vec<_>>();
                let compact =
                    overlay_content_height(&requested_sizes, OVERLAY_GAP) > column_bounds.height();
                let gap = if compact {
                    OVERLAY_COMPACT_GAP
                } else {
                    OVERLAY_GAP
                };
                if compact {
                    let content_height = overlay_content_height(&requested_sizes, gap).max(1.0);
                    let font_scale = (column_bounds.height() / content_height)
                        .clamp(OVERLAY_MIN_FONT_SCALE, 1.0);
                    if font_scale < 0.995 {
                        prepared = build_prepared(font_scale);
                        requested_sizes = prepared.iter().map(|(_, _, _, size)| *size).collect();
                    }
                }

                let visible_start =
                    visible_overlay_start(&requested_sizes, column_bounds.height(), gap);
                let horizontal = match layout_mode {
                    settings::OverlayLayoutMode::Reader
                    | settings::OverlayLayoutMode::Paragraph => {
                        overlay::HorizontalPlacement::Fixed(column_bounds.min.x)
                    }
                    settings::OverlayLayoutMode::Source => overlay::HorizontalPlacement::Source,
                };
                let stacked_boxes = overlay::stack_overlay_boxes(
                    &display_rects[visible_start..],
                    &requested_sizes[visible_start..],
                    column_bounds,
                    horizontal,
                    gap,
                );

                for (box_rect, (galley, text_color, pending, _)) in stacked_boxes
                    .into_iter()
                    .zip(prepared.into_iter().skip(visible_start))
                {
                    overlay_painter.rect(
                        box_rect,
                        5.0,
                        if pending {
                            Color32::from_rgba_unmultiplied(13, 25, 35, 210)
                        } else {
                            Color32::from_rgba_unmultiplied(14, 18, 26, 238)
                        },
                        Stroke::new(
                            1.0,
                            if pending {
                                Color32::from_rgba_unmultiplied(63, 192, 238, 115)
                            } else {
                                Color32::from_rgba_unmultiplied(63, 192, 238, 180)
                            },
                        ),
                        StrokeKind::Inside,
                    );
                    overlay_painter.galley(box_rect.min + Vec2::new(7.0, 4.0), galley, text_color);
                }

                if visible_start > 0 {
                    let unit = if layout_mode == settings::OverlayLayoutMode::Paragraph {
                        "문단"
                    } else {
                        "줄"
                    };
                    let hidden = format!("이전 {visible_start}{unit} 숨김");
                    painter.text(
                        column_bounds.right_top() + Vec2::new(-6.0, 6.0),
                        egui::Align2::RIGHT_TOP,
                        hidden,
                        FontId::proportional(11.0),
                        Color32::from_rgba_unmultiplied(196, 213, 231, 190),
                    );
                }

                if self.live_lines.is_empty() || self.error.is_some() {
                    let badge_text = if self.error.is_some() {
                        format!("재시도 중 · {SCREEN_HOTKEY_LABEL} 종료")
                    } else {
                        "문자 위치 찾는 중…".to_owned()
                    };
                    let badge_galley = painter.layout_no_wrap(
                        badge_text,
                        FontId::proportional(13.0),
                        Color32::from_rgb(226, 241, 250),
                    );
                    let badge_padding = Vec2::new(11.0, 7.0);
                    let badge_y = if selected_rect.min.y > 38.0 {
                        selected_rect.min.y - badge_galley.size().y - badge_padding.y * 2.0 - 5.0
                    } else {
                        selected_rect.min.y + 7.0
                    };
                    let badge_rect = Rect::from_min_size(
                        Pos2::new(selected_rect.min.x + 7.0, badge_y),
                        badge_galley.size() + badge_padding * 2.0,
                    );
                    painter.rect(
                        badge_rect,
                        8.0,
                        Color32::from_rgba_unmultiplied(10, 15, 23, 230),
                        Stroke::new(1.0, Color32::from_rgb(55, 190, 237)),
                        StrokeKind::Inside,
                    );
                    painter.galley(badge_rect.min + badge_padding, badge_galley, Color32::WHITE);
                }
            });
    }

    fn main_ui(&mut self, root_ui: &mut egui::Ui) {
        let ctx = root_ui.ctx().clone();
        let mut provider_choice = self.selected_provider;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(12, 16, 24))
                    .inner_margin(24.0),
            )
            .show(root_ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("main-content")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("Dog Translate")
                                .size(28.0)
                                .strong()
                                .color(Color32::from_rgb(238, 245, 255)),
                        );
                        ui.label(
                            egui::RichText::new("채팅 영역을 고르면 각 문장 자리에서 번역됩니다")
                                .size(14.0)
                                .color(Color32::from_rgb(145, 159, 182)),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let plan = self.account_plan.as_deref().unwrap_or("연결 중");
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(24, 42, 50))
                            .corner_radius(16.0)
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "●  {} {plan}",
                                        self.selected_provider.display_name()
                                    ))
                                        .size(12.0)
                                        .color(Color32::from_rgb(101, 225, 181)),
                                );
                            });
                    });
                });

                ui.add_space(20.0);
                egui::Frame::NONE
                    .fill(if self.error.is_some() {
                        Color32::from_rgb(54, 28, 34)
                    } else {
                        Color32::from_rgb(22, 29, 41)
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if self.error.is_some() {
                            Color32::from_rgb(137, 57, 71)
                        } else {
                            Color32::from_rgb(42, 53, 71)
                        },
                    ))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if self.background_busy {
                                ui.spinner();
                            } else if self.error.is_some() || self.hotkey_error.is_some() {
                                ui.label(
                                    egui::RichText::new("●")
                                        .color(Color32::from_rgb(255, 115, 126)),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("●")
                                        .color(Color32::from_rgb(74, 211, 164)),
                                );
                            }
                            ui.label(&self.status);
                            if let Some(timing) = &self.timing {
                                ui.weak(timing);
                            }
                        });
                        if let Some(error) = self.hotkey_error.as_ref().or(self.error.as_ref()) {
                            ui.add_space(4.0);
                            ui.colored_label(
                                Color32::from_rgb(255, 145, 151),
                                friendly_error_message(error),
                            )
                            .on_hover_text(error);
                            if self
                                .error
                                .as_ref()
                                .is_some_and(|error| !error.contains("화면 기록 권한"))
                                && ui
                                    .small_button(format!(
                                        "{} 다시 연결",
                                        self.selected_provider.display_name()
                                    ))
                                    .clicked()
                            {
                                self.error = None;
                                self.status = format!(
                                    "{} 계정 다시 연결 중…",
                                    self.selected_provider.display_name()
                                );
                                self.restart_worker();
                            }
                        }
                    });

                ui.add_space(12.0);
                egui::Frame::NONE
                    .fill(Color32::from_rgb(17, 23, 33))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(38, 49, 66)))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new("번역 계정")
                                    .size(12.0)
                                    .strong()
                                    .color(Color32::from_rgb(193, 205, 224)),
                            );
                            ui.add_space(8.0);
                            ui.add_enabled_ui(
                                self.phase == AppPhase::Ready && !self.background_busy,
                                |ui| {
                                    ui.selectable_value(
                                        &mut provider_choice,
                                        provider::ProviderKind::ChatGpt,
                                        "ChatGPT로 로그인",
                                    )
                                    .on_hover_text("ChatGPT Plus/Pro 등 Codex 사용 가능 계정");
                                    ui.selectable_value(
                                        &mut provider_choice,
                                        provider::ProviderKind::Claude,
                                        "Claude로 로그인",
                                    )
                                    .on_hover_text("Claude Pro/Max/Team/Enterprise 계정");
                                },
                            );
                        });
                        ui.label(
                            egui::RichText::new(match self.selected_provider {
                                provider::ProviderKind::ChatGpt => {
                                    "브라우저에서 ChatGPT에 로그인하며 API 키는 필요하지 않습니다."
                                }
                                provider::ProviderKind::Claude => {
                                    "공식 Claude Code 로그인과 구독 사용량을 이용합니다."
                                }
                            })
                            .size(11.0)
                            .color(Color32::from_rgb(111, 126, 148)),
                        );
                    });
                if provider_choice != self.selected_provider {
                    self.change_provider(provider_choice);
                }

                ui.add_space(12.0);
                let mut model_choice = self.selected_model.clone();
                let mut reasoning_choice = self.selected_reasoning_effort.clone();
                let mut layout_choice = self.overlay_layout;
                let reasoning_options = self
                    .available_models
                    .iter()
                    .find(|model| model.id == self.selected_model)
                    .map(|model| model.reasoning_efforts.clone())
                    .unwrap_or_default();
                let selector_enabled = self.phase == AppPhase::Ready
                    && !self.background_busy
                    && self.account_plan.is_some()
                    && !self.available_models.is_empty();
                egui::Frame::NONE
                    .fill(Color32::from_rgb(17, 23, 33))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(38, 49, 66)))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::symmetric(14, 9))
                    .show(ui, |ui| {
                        let summary = format!(
                            "{} · {} · {}",
                            model_display_name(&self.available_models, &self.selected_model),
                            reasoning_display_name(&self.selected_reasoning_effort),
                            overlay_layout_display_name(self.overlay_layout)
                        );
                        let marker = if self.advanced_settings_open {
                            "▼"
                        } else {
                            "▶"
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(format!(
                                        "{marker}  고급 설정  ·  {summary}"
                                    ))
                                    .size(13.0)
                                    .strong()
                                    .color(Color32::from_rgb(205, 216, 233)),
                                )
                                .frame(false),
                            )
                            .on_hover_text("모델과 속도·정확도 설정 열기/닫기")
                            .clicked()
                        {
                            self.advanced_settings_open = !self.advanced_settings_open;
                        }
                        if self.advanced_settings_open {
                            ui.add_space(5.0);
                            ui.label(
                                egui::RichText::new(
                                    "평소에는 속도 우선을 권장합니다. 문맥이 자주 어긋날 때만 단계를 높여 보세요.",
                                )
                                .size(11.0)
                                .color(Color32::from_rgb(111, 126, 148)),
                            );
                            ui.add_space(8.0);
                            ui.add_enabled_ui(selector_enabled, |ui| {
                                ui.columns(2, |columns| {
                                    columns[0].label(
                                        egui::RichText::new("모델")
                                            .size(11.0)
                                            .color(Color32::from_rgb(139, 153, 174)),
                                    );
                                    egui::ComboBox::from_id_salt("translation-model")
                                        .width(columns[0].available_width())
                                        .selected_text(model_display_name(
                                            &self.available_models,
                                            &self.selected_model,
                                        ))
                                        .show_ui(&mut columns[0], |ui| {
                                            for model in &self.available_models {
                                                ui.selectable_value(
                                                    &mut model_choice,
                                                    model.id.clone(),
                                                    &model.display_name,
                                                )
                                                .on_hover_text(format!(
                                                    "모델 ID: {} · 지원 단계: {}",
                                                    model.id,
                                                    model.reasoning_efforts.join(", ")
                                                ));
                                            }
                                        });

                                    columns[1].label(
                                        egui::RichText::new("속도·정확도")
                                            .size(11.0)
                                            .color(Color32::from_rgb(139, 153, 174)),
                                    );
                                    egui::ComboBox::from_id_salt("reasoning-effort")
                                        .width(columns[1].available_width())
                                        .selected_text(reasoning_display_name(
                                            &self.selected_reasoning_effort,
                                        ))
                                        .show_ui(&mut columns[1], |ui| {
                                            for effort in &reasoning_options {
                                                ui.selectable_value(
                                                    &mut reasoning_choice,
                                                    effort.clone(),
                                                    reasoning_display_name(effort),
                                                )
                                                .on_hover_text(format!("추론 단계: {effort}"));
                                            }
                                        });
                                });
                            });
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(7.0);
                            ui.label(
                                egui::RichText::new("화면 번역 배치")
                                    .size(11.0)
                                    .color(Color32::from_rgb(139, 153, 174)),
                            );
                            egui::ComboBox::from_id_salt("overlay-layout")
                                .width(ui.available_width())
                                .selected_text(overlay_layout_display_name(layout_choice))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut layout_choice,
                                        settings::OverlayLayoutMode::Reader,
                                        "독서형 · 전체 가로 폭",
                                    )
                                    .on_hover_text(
                                        "선택 영역의 가로 폭을 최대한 사용해 위에서 아래로 읽습니다",
                                    );
                                    ui.selectable_value(
                                        &mut layout_choice,
                                        settings::OverlayLayoutMode::Paragraph,
                                        "문단형 · 문단별 묶기",
                                    )
                                    .on_hover_text(
                                        "줄 간격과 정렬을 분석해 같은 문단의 번역을 한 블록으로 묶습니다",
                                    );
                                    ui.selectable_value(
                                        &mut layout_choice,
                                        settings::OverlayLayoutMode::Source,
                                        "원문 덮기형 · OCR 위치",
                                    )
                                    .on_hover_text(
                                        "각 OCR 줄의 위치와 폭을 기준으로 번역문을 덮습니다",
                                    );
                                });
                        }
                    });
                if model_choice != self.selected_model {
                    self.change_model(model_choice);
                } else if reasoning_choice != self.selected_reasoning_effort {
                    self.change_reasoning_effort(reasoning_choice);
                }
                if layout_choice != self.overlay_layout {
                    self.change_overlay_layout(layout_choice);
                }

                ui.add_space(18.0);
                ui.label(
                    egui::RichText::new("빠른 작업")
                        .size(15.0)
                        .strong()
                        .color(Color32::from_rgb(193, 205, 224)),
                );
                ui.add_space(8.0);

                if action_card(
                    ui,
                    "선택 영역 실시간 번역",
                    "채팅 영역을 드래그하면 각 문장 위치에서 바로 번역",
                    SCREEN_HOTKEY_LABEL,
                    Color32::from_rgb(67, 177, 240),
                )
                .clicked()
                {
                    self.begin_capture(&ctx);
                }
                ui.horizontal(|ui| {
                    ui.weak("각 위치에 번역 중 표시 · 완료 후 한국어로 교체 · 클릭 통과");
                });

                ui.add_space(10.0);
                let selection_translation_busy = self.clipboard_operation.is_some();
                let selection_card_detail = if selection_translation_busy {
                    "진행 중인 번역을 중단하고 결과를 버립니다".to_owned()
                } else {
                    format!(
                        "{SELECT_ALL_LABEL}로 선택하고 {SELECTION_CLIPBOARD_HOTKEY_LABEL} · 완료 알림 후 {PASTE_LABEL}로 붙여넣기"
                    )
                };
                if action_card(
                    ui,
                    if selection_translation_busy {
                        "선택문 번역 취소"
                    } else {
                        "선택문 번역 → 붙여넣기"
                    },
                    &selection_card_detail,
                    SELECTION_CLIPBOARD_HOTKEY_LABEL,
                    if selection_translation_busy {
                        Color32::from_rgb(245, 181, 72)
                    } else {
                        Color32::from_rgb(139, 112, 246)
                    },
                )
                .clicked()
                {
                    if selection_translation_busy {
                        self.cancel_selection_translation();
                    } else {
                        self.status = format!(
                            "다른 앱에서 문장을 선택한 뒤 {SELECTION_CLIPBOARD_HOTKEY_LABEL}를 누르세요"
                        );
                    }
                }

                    });
            });
    }
}

impl eframe::App for TranslatorApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_escape(ctx);
        self.handle_hotkey(ctx);
        self.receive_capture(ctx);
        self.receive_ocr();
        self.receive_worker();
        self.update_translation_toast(ctx);
        self.update_live_wait_status();
        self.schedule_live_capture(ctx);

        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_translation_toast(ui.ctx());
        if self.root_viewport_hidden {
            return;
        }
        match self.phase {
            AppPhase::Selecting => self.selection_ui(ui),
            AppPhase::Live => self.live_ui(ui),
            _ => self.main_ui(ui),
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
}

impl Drop for TranslatorApp {
    fn drop(&mut self) {
        self.cancel_active_clipboard_operation();
        self.cancel_active_live_operation();
        if let Some(manager) = &self.hotkey_manager {
            for hotkey in [
                self.screen_hotkey,
                self.selection_clipboard_hotkey,
                self.escape_hotkey,
            ] {
                let _ = manager.unregister(hotkey);
            }
        }
    }
}

fn spawn_worker() -> (Sender<WorkerJob>, Receiver<WorkerEvent>) {
    let (job_tx, job_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let crash_events = event_tx.clone();

    thread::spawn(move || {
        if catch_unwind(AssertUnwindSafe(|| run_worker(job_rx, event_tx))).is_err() {
            let _ = crash_events.send(WorkerEvent::Crashed);
        }
    });

    (job_tx, event_rx)
}

fn spawn_ocr_worker() -> (Sender<OcrJob>, Receiver<OcrEvent>) {
    let (job_tx, job_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let crash_events = event_tx.clone();

    thread::spawn(move || {
        if catch_unwind(AssertUnwindSafe(|| run_ocr_worker(job_rx, event_tx))).is_err() {
            let _ = crash_events.send(OcrEvent::Crashed);
        }
    });

    (job_tx, event_rx)
}

fn run_ocr_worker(job_rx: Receiver<OcrJob>, event_tx: Sender<OcrEvent>) {
    while let Ok(job) = job_rx.recv() {
        match job {
            OcrJob::RecognizeLiveFrame { image, session } => {
                let started = Instant::now();
                match ocr::recognize_english_lines(&image) {
                    Ok(lines) => {
                        let _ = event_tx.send(OcrEvent::Recognized {
                            lines,
                            elapsed: started.elapsed(),
                            session,
                        });
                    }
                    Err(error) => {
                        let _ = event_tx.send(OcrEvent::Failed {
                            message: format!("{error:#}"),
                            session,
                        });
                    }
                }
            }
        }
    }
}

fn run_worker(job_rx: Receiver<WorkerJob>, event_tx: Sender<WorkerEvent>) {
    let mut client: Option<provider::LlmClient> = None;
    let mut active_provider = provider::ProviderKind::ChatGpt;
    let mut active_model = provider::default_model(active_provider).to_owned();
    let mut active_reasoning_effort = Some(default_reasoning_for_model(&active_model));

    while let Ok(job) = job_rx.recv() {
        match job {
            WorkerJob::Connect {
                provider,
                model,
                reasoning_effort,
            } => {
                active_provider = provider;
                active_model = model;
                active_reasoning_effort = reasoning_effort;
                match connect_worker_client(
                    active_provider,
                    &active_model,
                    active_reasoning_effort.as_deref(),
                    &event_tx,
                ) {
                    Ok(connected) => {
                        active_model = connected.model().to_owned();
                        active_reasoning_effort = Some(connected.reasoning_effort().to_owned());
                        client = Some(connected);
                    }
                    Err(error) => {
                        let _ = event_tx.send(WorkerEvent::Failed(format!("{error:#}")));
                    }
                }
            }
            WorkerJob::TranslateLiveLines {
                targets,
                context_lines,
                session,
                cancelled,
            } => {
                if cancelled.load(Ordering::Acquire) {
                    continue;
                }
                let sources = targets
                    .iter()
                    .map(|target| target.source.clone())
                    .collect::<Vec<_>>();
                if client.is_none() {
                    client = match connect_worker_client(
                        active_provider,
                        &active_model,
                        active_reasoning_effort.as_deref(),
                        &event_tx,
                    ) {
                        Ok(connected) => {
                            active_model = connected.model().to_owned();
                            active_reasoning_effort = Some(connected.reasoning_effort().to_owned());
                            Some(connected)
                        }
                        Err(error) => {
                            let _ = event_tx.send(WorkerEvent::LiveFailed {
                                message: format!("{error:#}"),
                                session,
                            });
                            continue;
                        }
                    };
                }
                if cancelled.load(Ordering::Acquire) {
                    client = None;
                    continue;
                }

                let Some(connected) = client.as_mut() else {
                    let _ = event_tx.send(WorkerEvent::LiveFailed {
                        message: "번역 연결 초기화에 실패했습니다".to_owned(),
                        session,
                    });
                    continue;
                };
                match connected.translate_batch_cancellable(
                    &sources,
                    &context_lines,
                    openai::TranslationDirection::EnglishToKorean,
                    &cancelled,
                ) {
                    Ok(Some(batch)) => {
                        let translations = targets
                            .into_iter()
                            .zip(batch.texts)
                            .filter_map(|(target, translation)| {
                                (!translation.trim().is_empty())
                                    .then_some((target.key, translation))
                            })
                            .collect();
                        let _ = event_tx.send(WorkerEvent::LiveTranslations {
                            translations,
                            elapsed: batch.wall_time,
                            session,
                        });
                    }
                    Ok(None) => {
                        client = None;
                    }
                    Err(error) => {
                        client = None;
                        if !cancelled.load(Ordering::Acquire) {
                            let _ = event_tx.send(WorkerEvent::LiveFailed {
                                message: format!("{error:#}"),
                                session,
                            });
                        }
                    }
                }
            }
            WorkerJob::TranslateSelection { cancelled } => {
                let started = Instant::now();
                if cancelled.load(Ordering::Acquire) {
                    continue;
                }
                let _ = event_tx.send(WorkerEvent::Status("선택한 한국어를 읽는 중…".to_owned()));
                let source = clipboard::capture_selected_text();
                let source = match source {
                    Ok(source) => source,
                    Err(error) => {
                        let _ = event_tx.send(WorkerEvent::Failed(format!("{error:#}")));
                        continue;
                    }
                };
                if cancelled.load(Ordering::Acquire) {
                    continue;
                }

                let _ = event_tx.send(WorkerEvent::Status(format!(
                    "한국어 → 영어 · {} {} · 추론 {} 번역 중…",
                    active_provider.display_name(),
                    active_model,
                    active_reasoning_effort.as_deref().unwrap_or("자동")
                )));
                if client.is_none() {
                    client = match connect_worker_client(
                        active_provider,
                        &active_model,
                        active_reasoning_effort.as_deref(),
                        &event_tx,
                    ) {
                        Ok(connected) => {
                            active_model = connected.model().to_owned();
                            active_reasoning_effort = Some(connected.reasoning_effort().to_owned());
                            Some(connected)
                        }
                        Err(error) => {
                            let _ = event_tx.send(WorkerEvent::Failed(format!("{error:#}")));
                            continue;
                        }
                    };
                }
                if cancelled.load(Ordering::Acquire) {
                    client = None;
                    continue;
                }

                let Some(connected) = client.as_mut() else {
                    let _ = event_tx.send(WorkerEvent::Failed(
                        "번역 연결 초기화에 실패했습니다".to_owned(),
                    ));
                    continue;
                };
                let translation = connected.translate_cancellable(
                    &source,
                    openai::TranslationDirection::KoreanToEnglish,
                    &cancelled,
                );
                match translation {
                    Ok(Some(result)) => {
                        if cancelled.load(Ordering::Acquire) {
                            client = None;
                            continue;
                        }
                        let delivery = match clipboard::write_text(&result.text) {
                            Ok(()) => Delivery::SelectionClipboard,
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::Failed(format!("{error:#}")));
                                continue;
                            }
                        };
                        let _ = event_tx.send(WorkerEvent::Complete {
                            elapsed: started.elapsed(),
                            delivery,
                        });
                    }
                    Ok(None) => {
                        client = None;
                    }
                    Err(error) => {
                        client = None;
                        if !cancelled.load(Ordering::Acquire) {
                            let _ = event_tx.send(WorkerEvent::Failed(format!("{error:#}")));
                        }
                    }
                }
            }
        }
    }
}

fn connect_worker_client(
    provider: provider::ProviderKind,
    requested_model: &str,
    requested_reasoning_effort: Option<&str>,
    event_tx: &Sender<WorkerEvent>,
) -> anyhow::Result<provider::LlmClient> {
    let events = event_tx.clone();
    let connected = provider::LlmClient::connect(
        provider,
        requested_model,
        requested_reasoning_effort,
        move |status| {
            let _ = events.send(WorkerEvent::Status(status.to_owned()));
        },
    )?;
    let _ = event_tx.send(WorkerEvent::Connected {
        plan_type: connected.plan_type().to_owned(),
        selected_model: connected.model().to_owned(),
        selected_reasoning_effort: connected.reasoning_effort().to_owned(),
        models: connected.available_models().to_vec(),
    });
    Ok(connected)
}

fn live_text_key(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn reconcile_live_lines(
    previous: &[LiveOverlayLine],
    recognized: Vec<ocr::RecognizedLine>,
    cache: &HashMap<String, String>,
    next_id: &mut u64,
) -> Vec<LiveOverlayLine> {
    let mut used_previous = vec![false; previous.len()];
    recognized
        .into_iter()
        .map(|line| {
            let key = live_text_key(&line.text);
            let matched = previous
                .iter()
                .enumerate()
                .filter(|(index, candidate)| {
                    !used_previous[*index] && live_text_key(&candidate.source) == key
                })
                .min_by(|(_, left), (_, right)| {
                    let left_distance = (left.x - line.x).hypot(left.y - line.y);
                    let right_distance = (right.x - line.x).hypot(right.y - line.y);
                    left_distance.total_cmp(&right_distance)
                })
                .map(|(index, candidate)| (index, candidate.id));
            let id = if let Some((index, id)) = matched {
                used_previous[index] = true;
                id
            } else {
                let id = *next_id;
                *next_id = next_id.wrapping_add(1).max(1);
                id
            };
            let translation = cache.get(&key).cloned().unwrap_or_default();
            LiveOverlayLine {
                id,
                source: line.text,
                translation: translation.clone(),
                x: line.x,
                y: line.y,
                width: line.width,
                height: line.height,
                pending: translation.is_empty(),
            }
        })
        .collect()
}

fn live_retry_delay(failure_streak: u32) -> Duration {
    let exponent = failure_streak.saturating_sub(1).min(4);
    (LIVE_SCAN_INTERVAL * (1_u32 << exponent)).min(LIVE_RETRY_MAX)
}

fn default_reasoning_for_model(model: &str) -> String {
    if model == openai::DEFAULT_MODEL {
        "none".to_owned()
    } else {
        "low".to_owned()
    }
}

fn format_metrics(elapsed: Duration) -> String {
    format!("· {:.1}초", elapsed.as_secs_f64())
}

fn overlay_column_bounds(selected: Rect, viewport: Rect) -> Rect {
    let selected = selected.intersect(viewport);
    let inset = if selected.width() >= 80.0 { 4.0 } else { 0.0 };
    Rect::from_min_max(
        Pos2::new(selected.min.x + inset, viewport.min.y),
        Pos2::new(selected.max.x - inset, viewport.max.y),
    )
}

fn detect_paragraph_ranges(lines: &[&LiveOverlayLine], rects: &[Rect]) -> Vec<(usize, usize)> {
    let count = lines.len().min(rects.len());
    if count == 0 {
        return Vec::new();
    }

    let mut heights = rects[..count].iter().map(Rect::height).collect::<Vec<_>>();
    heights.sort_by(f32::total_cmp);
    let typical_height = heights[heights.len() / 2].max(1.0);
    let same_column = |left: Rect, right: Rect| {
        let overlap = (left.max.x.min(right.max.x) - left.min.x.max(right.min.x)).max(0.0);
        let overlap_ratio = overlap / left.width().min(right.width()).max(1.0);
        overlap_ratio >= 0.25
            || (left.min.x - right.min.x).abs() <= (typical_height * 1.75).max(24.0)
    };

    let mut normal_gaps = Vec::new();
    for pair in rects[..count].windows(2) {
        let gap = pair[1].min.y - pair[0].max.y;
        if same_column(pair[0], pair[1]) && gap >= 0.0 && gap <= typical_height * 1.25 {
            normal_gaps.push(gap);
        }
    }
    normal_gaps.sort_by(f32::total_cmp);
    let typical_gap = normal_gaps
        .get(normal_gaps.len() / 2)
        .copied()
        .unwrap_or(typical_height * 0.25);
    let paragraph_gap = (typical_gap * 1.9)
        .max(typical_height * 0.7)
        .min(typical_height * 1.5);

    let mut ranges = Vec::new();
    let mut start = 0;
    for index in 1..count {
        let previous = rects[index - 1];
        let current = rects[index];
        let vertical_gap = current.min.y - previous.max.y;
        let indentation = current.min.x - previous.min.x;
        let indented_new_sentence =
            indentation > typical_height * 2.5 && ends_sentence(&lines[index - 1].source);
        if !same_column(previous, current) || vertical_gap > paragraph_gap || indented_new_sentence
        {
            ranges.push((start, index));
            start = index;
        }
    }
    ranges.push((start, count));
    ranges
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .chars()
        .next_back()
        .is_some_and(|last| matches!(last, '.' | '!' | '?' | '。' | '！' | '？'))
}

fn union_rects(rects: &[Rect]) -> Rect {
    let Some((&first, rest)) = rects.split_first() else {
        return Rect::NOTHING;
    };
    rest.iter()
        .fold(first, |combined, rect| combined.union(*rect))
}

fn overlay_wrap_width(column_width: f32) -> f32 {
    (column_width - 14.0).max(1.0)
}

fn overlay_block_width(
    layout: settings::OverlayLayoutMode,
    source_width: f32,
    column_width: f32,
) -> f32 {
    match layout {
        settings::OverlayLayoutMode::Reader => column_width,
        settings::OverlayLayoutMode::Paragraph => column_width,
        settings::OverlayLayoutMode::Source => {
            source_width.clamp(96.0_f32.min(column_width), column_width)
        }
    }
}

fn overlay_block_size(text_size: Vec2, column_width: f32, fill_width: bool) -> Vec2 {
    let padded = text_size + Vec2::new(14.0, 8.0);
    Vec2::new(
        if fill_width {
            column_width
        } else {
            padded.x.min(column_width)
        },
        padded.y,
    )
}

fn overlay_content_height(sizes: &[Vec2], gap: f32) -> f32 {
    sizes.iter().map(|size| size.y).sum::<f32>() + gap * sizes.len().saturating_sub(1) as f32
}

fn visible_overlay_start(sizes: &[Vec2], available_height: f32, gap: f32) -> usize {
    let mut used = 0.0;
    let mut start = sizes.len();
    for (index, size) in sizes.iter().enumerate().rev() {
        let required = size.y + if start < sizes.len() { gap } else { 0.0 };
        if used + required > available_height && start < sizes.len() {
            break;
        }
        used += required;
        start = index;
    }
    start
}

fn overlay_layout_display_name(layout: settings::OverlayLayoutMode) -> &'static str {
    match layout {
        settings::OverlayLayoutMode::Reader => "독서형",
        settings::OverlayLayoutMode::Paragraph => "문단형",
        settings::OverlayLayoutMode::Source => "원문 덮기형",
    }
}

fn format_overlay_translation(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn escape_cancels_selection_and_an_active_overlay() {
        assert_eq!(escape_action_for_phase(AppPhase::Ready), None);
        assert_eq!(
            escape_action_for_phase(AppPhase::Capturing),
            Some(EscapeAction::CancelSelection)
        );
        assert_eq!(
            escape_action_for_phase(AppPhase::Selecting),
            Some(EscapeAction::CancelSelection)
        );
        assert_eq!(
            escape_action_for_phase(AppPhase::Live),
            Some(EscapeAction::StopLive)
        );
    }

    #[test]
    fn moves_long_translation_away_from_previous_box() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0));
        let occupied = [Rect::from_min_size(
            Pos2::new(100.0, 100.0),
            Vec2::new(520.0, 90.0),
        )];
        let source = Rect::from_min_size(Pos2::new(110.0, 135.0), Vec2::new(180.0, 20.0));
        let placed = overlay::place_overlay_box(
            source,
            Vec2::new(560.0, 110.0),
            bounds,
            &occupied,
            &[source],
        );

        assert!(!placed.expand(2.0).intersects(occupied[0]));
        assert!(bounds.contains_rect(placed));
    }

    #[test]
    fn keeps_translation_box_inside_monitor() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 360.0));
        let source = Rect::from_min_size(Pos2::new(590.0, 320.0), Vec2::new(45.0, 18.0));
        let placed =
            overlay::place_overlay_box(source, Vec2::new(500.0, 90.0), bounds, &[], &[source]);

        assert!(bounds.contains_rect(placed));
    }

    #[test]
    fn live_retry_uses_bounded_exponential_backoff() {
        assert_eq!(live_retry_delay(1), Duration::from_millis(700));
        assert_eq!(live_retry_delay(2), Duration::from_millis(1_400));
        assert_eq!(live_retry_delay(5), LIVE_RETRY_MAX);
        assert_eq!(live_retry_delay(100), LIVE_RETRY_MAX);
    }

    #[test]
    fn uses_the_entire_selected_row_before_wrapping() {
        assert_eq!(overlay_wrap_width(640.0), 626.0);
        assert_eq!(overlay_wrap_width(250.0), 236.0);
        assert_eq!(overlay_wrap_width(180.0), 166.0);

        assert_eq!(
            overlay_block_size(Vec2::new(80.0, 20.0), 250.0, false),
            Vec2::new(94.0, 28.0)
        );
        assert_eq!(
            overlay_block_size(Vec2::new(300.0, 20.0), 250.0, false),
            Vec2::new(250.0, 28.0)
        );
        assert_eq!(
            overlay_block_width(settings::OverlayLayoutMode::Reader, 120.0, 400.0),
            400.0
        );
        assert_eq!(
            overlay_block_width(settings::OverlayLayoutMode::Source, 120.0, 400.0),
            120.0
        );

        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1_000.0, 700.0));
        let selected = Rect::from_min_max(Pos2::new(200.0, 100.0), Pos2::new(600.0, 500.0));
        let column = overlay_column_bounds(selected, viewport);
        assert_eq!(column.min.x, 204.0);
        assert_eq!(column.max.x, 596.0);
    }

    #[test]
    fn keeps_one_ocr_line_as_one_translation_block() {
        assert_eq!(
            format_overlay_translation("첫 번째 절입니다. 다음 절은 아래에서 이어집니다."),
            "첫 번째 절입니다. 다음 절은 아래에서 이어집니다."
        );
        assert_eq!(
            format_overlay_translation("첫 번째 줄\n두 번째 줄"),
            "첫 번째 줄 두 번째 줄"
        );
    }

    #[test]
    fn keeps_line_identity_and_translation_when_only_position_changes() {
        let previous = [LiveOverlayLine {
            id: 7,
            source: "same sentence".to_owned(),
            translation: "같은 문장".to_owned(),
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 18.0,
            pending: false,
        }];
        let recognized = vec![ocr::RecognizedLine {
            text: "same  sentence".to_owned(),
            x: 13.0,
            y: 24.0,
            width: 102.0,
            height: 18.0,
        }];
        let cache = HashMap::from([("same sentence".to_owned(), "같은 문장".to_owned())]);
        let mut next_id = 8;
        let reconciled = reconcile_live_lines(&previous, recognized, &cache, &mut next_id);

        assert_eq!(reconciled[0].id, 7);
        assert_eq!(reconciled[0].translation, "같은 문장");
        assert!(!reconciled[0].pending);
        assert_eq!(next_id, 8);
    }

    #[test]
    fn hides_oldest_lines_only_after_compacting_the_stack() {
        let sizes = vec![Vec2::new(100.0, 30.0); 4];
        assert_eq!(overlay_content_height(&sizes, 2.0), 126.0);
        assert_eq!(visible_overlay_start(&sizes, 70.0, 2.0), 2);
    }

    #[test]
    fn detects_paragraphs_from_line_spacing_and_columns() {
        let make_line = |id, text: &str, x, y| LiveOverlayLine {
            id,
            source: text.to_owned(),
            translation: String::new(),
            x,
            y,
            width: 180.0,
            height: 18.0,
            pending: true,
        };
        let lines = [
            make_line(1, "First line", 20.0, 0.0),
            make_line(2, "Second line", 20.0, 22.0),
            make_line(3, "End of paragraph.", 20.0, 44.0),
            make_line(4, "New paragraph", 20.0, 90.0),
            make_line(5, "Same paragraph", 20.0, 112.0),
            make_line(6, "Different column", 320.0, 134.0),
        ];
        let references = lines.iter().collect::<Vec<_>>();
        let rects = lines
            .iter()
            .map(|line| {
                Rect::from_min_size(
                    Pos2::new(line.x, line.y),
                    Vec2::new(line.width, line.height),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            detect_paragraph_ranges(&references, &rects),
            vec![(0, 3), (3, 5), (5, 6)]
        );
    }
}
