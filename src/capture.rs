#[cfg(not(target_os = "windows"))]
use anyhow::bail;
use anyhow::{Context, Result};
use xcap::{Monitor, image::RgbaImage};

#[cfg(target_os = "windows")]
use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};

#[cfg(target_os = "macos")]
use std::{
    collections::HashMap,
    ffi::c_void,
    process::Command,
    ptr,
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(target_os = "macos")]
use screencapturekit::{
    cg::CGRect,
    screenshot_manager::{CGImageExt, SCScreenshotManager},
    shareable_content::SCShareableContent,
    stream::{configuration::SCStreamConfiguration, content_filter::SCContentFilter},
};

#[cfg(target_os = "macos")]
const SCREEN_CAPTURE_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

#[cfg(target_os = "macos")]
#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *const c_void) -> CGPoint;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

pub struct CapturedScreen {
    pub image: RgbaImage,
    pub x: i32,
    pub y: i32,
    pub scale_factor: f32,
}

#[cfg(target_os = "macos")]
pub fn screen_capture_permission_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(target_os = "macos")]
pub fn ensure_screen_capture_permission() -> Result<()> {
    if screen_capture_permission_granted() {
        return Ok(());
    }

    // This displays macOS's native Screen Recording permission prompt the first
    // time capture is requested. If access was denied before, macOS does not
    // show the prompt again, so provide the exact Settings path below instead.
    if unsafe { CGRequestScreenCaptureAccess() } {
        return Ok(());
    }

    let settings_result = Command::new("/usr/bin/open")
        .arg(SCREEN_CAPTURE_SETTINGS_URL)
        .spawn()
        .context("화면 기록 권한 설정을 자동으로 열지 못했습니다");

    match settings_result {
        Ok(_) => bail!(
            "macOS 화면 기록 권한이 필요하여 시스템 설정을 열었습니다. Dog Translate를 허용한 뒤 앱을 다시 실행하세요."
        ),
        Err(error) => bail!(
            "macOS 화면 기록 권한이 필요합니다. 시스템 설정 > 개인정보 보호 및 보안 > 화면 및 시스템 오디오 기록에서 Dog Translate를 허용한 뒤 앱을 다시 실행하세요. 설정을 자동으로 열지 못했습니다: {error:#}"
        ),
    }
}

#[cfg(target_os = "macos")]
fn screen_capture_filter(display_id: u32) -> Result<Arc<SCContentFilter>> {
    static FILTERS: OnceLock<Mutex<HashMap<u32, Arc<SCContentFilter>>>> = OnceLock::new();
    let filters = FILTERS.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(filter) = filters
        .lock()
        .map_err(|_| anyhow::anyhow!("화면 캡처 필터 잠금이 손상되었습니다"))?
        .get(&display_id)
        .cloned()
    {
        return Ok(filter);
    }

    let content = SCShareableContent::get().map_err(|error| {
        anyhow::anyhow!("ScreenCaptureKit 화면 목록을 읽지 못했습니다: {error}")
    })?;
    let display = content
        .displays()
        .into_iter()
        .find(|display| display.display_id() == display_id)
        .with_context(|| {
            format!("ScreenCaptureKit에서 디스플레이 {display_id}를 찾지 못했습니다")
        })?;
    let filter = Arc::new(
        SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build(),
    );
    filters
        .lock()
        .map_err(|_| anyhow::anyhow!("화면 캡처 필터 잠금이 손상되었습니다"))?
        .insert(display_id, filter.clone());
    Ok(filter)
}

#[cfg(target_os = "macos")]
fn capture_with_screen_capture_kit(
    display_id: u32,
    width: u32,
    height: u32,
    source_rect: Option<CGRect>,
) -> Result<RgbaImage> {
    let filter = screen_capture_filter(display_id)?;
    let mut configuration = SCStreamConfiguration::new()
        .with_width(width.max(1))
        .with_height(height.max(1));
    if let Some(source_rect) = source_rect {
        configuration.set_source_rect(source_rect);
    }

    let captured = SCScreenshotManager::capture_image(&filter, &configuration)
        .map_err(|error| anyhow::anyhow!("ScreenCaptureKit 화면 캡처에 실패했습니다: {error}"))?;
    let captured_width =
        u32::try_from(captured.width()).context("캡처 이미지 너비가 너무 큽니다")?;
    let captured_height =
        u32::try_from(captured.height()).context("캡처 이미지 높이가 너무 큽니다")?;
    let pixels = captured
        .rgba_data()
        .map_err(|error| anyhow::anyhow!("ScreenCaptureKit 픽셀 변환에 실패했습니다: {error}"))?;
    RgbaImage::from_raw(captured_width, captured_height, pixels)
        .context("ScreenCaptureKit 캡처 픽셀 크기가 올바르지 않습니다")
}

#[cfg(target_os = "macos")]
fn screen_capture_kit_is_unavailable(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}");
    message.contains("requires macOS 14") || message.contains("requires a macOS 14")
}

#[cfg(not(target_os = "macos"))]
pub fn screen_capture_permission_granted() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_screen_capture_permission() -> Result<()> {
    Ok(())
}

pub fn capture_monitor_under_cursor() -> Result<CapturedScreen> {
    #[cfg(target_os = "windows")]
    {
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point) }.context("마우스 위치를 가져오지 못했습니다")?;

        capture_monitor_at(point.x, point.y)
    }

    #[cfg(target_os = "macos")]
    {
        let event = unsafe { CGEventCreate(ptr::null()) };
        if event.is_null() {
            bail!("macOS 마우스 위치를 가져오지 못했습니다");
        }
        let point = unsafe { CGEventGetLocation(event) };
        unsafe { CFRelease(event) };
        capture_monitor_at(point.x.round() as i32, point.y.round() as i32)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    bail!("화면 캡처는 현재 Windows와 macOS에서 지원합니다")
}

pub fn capture_monitor_at(x: i32, y: i32) -> Result<CapturedScreen> {
    let screen = Monitor::from_point(x, y).context("지정한 모니터를 찾지 못했습니다")?;
    let screen_x = screen.x().context("모니터 가로 위치를 읽지 못했습니다")?;
    let screen_y = screen.y().context("모니터 세로 위치를 읽지 못했습니다")?;
    let scale_factor = screen
        .scale_factor()
        .context("모니터 배율을 읽지 못했습니다")?
        .max(1.0);

    #[cfg(target_os = "macos")]
    let image = {
        let screen_id = screen.id().context("모니터 식별자를 읽지 못했습니다")?;
        let screen_width = screen.width().context("모니터 너비를 읽지 못했습니다")?;
        let screen_height = screen.height().context("모니터 높이를 읽지 못했습니다")?;
        let width = ((screen_width as f32 * scale_factor).round() as u32).max(1);
        let height = ((screen_height as f32 * scale_factor).round() as u32).max(1);
        match capture_with_screen_capture_kit(screen_id, width, height, None) {
            Ok(image) => image,
            Err(error) if screen_capture_kit_is_unavailable(&error) => {
                screen.capture_image().context("화면 캡처에 실패했습니다")?
            }
            Err(error) => return Err(error),
        }
    };
    #[cfg(not(target_os = "macos"))]
    let image = screen.capture_image().context("화면 캡처에 실패했습니다")?;

    #[cfg(target_os = "windows")]
    let (capture_x, capture_y) = (screen_x, screen_y);
    #[cfg(target_os = "macos")]
    let (capture_x, capture_y) = (
        (screen_x as f32 * scale_factor).round() as i32,
        (screen_y as f32 * scale_factor).round() as i32,
    );
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let (capture_x, capture_y) = (screen_x, screen_y);

    Ok(CapturedScreen {
        image,
        x: capture_x,
        y: capture_y,
        scale_factor,
    })
}

pub fn capture_monitor_region(
    monitor_x: i32,
    monitor_y: i32,
    region_x: i32,
    region_y: i32,
    width: u32,
    height: u32,
) -> Result<RgbaImage> {
    #[cfg(target_os = "windows")]
    let screen = Monitor::from_point(monitor_x + 1, monitor_y + 1)
        .context("선택 영역의 모니터를 찾지 못했습니다")?;

    #[cfg(target_os = "windows")]
    {
        let region_x = u32::try_from(region_x).context("영역 가로 좌표가 잘못되었습니다")?;
        let region_y = u32::try_from(region_y).context("영역 세로 좌표가 잘못되었습니다")?;
        screen
            .capture_region(region_x, region_y, width, height)
            .context("선택 영역 캡처에 실패했습니다")
    }

    #[cfg(target_os = "macos")]
    {
        let screen = Monitor::all()
            .context("macOS 모니터 목록을 가져오지 못했습니다")?
            .into_iter()
            .find(|candidate| {
                let Ok(x) = candidate.x() else {
                    return false;
                };
                let Ok(y) = candidate.y() else {
                    return false;
                };
                let Ok(scale) = candidate.scale_factor() else {
                    return false;
                };
                let scale = scale.max(1.0);
                (x as f32 * scale).round() as i32 == monitor_x
                    && (y as f32 * scale).round() as i32 == monitor_y
            })
            .context("선택 영역의 모니터를 다시 찾지 못했습니다")?;
        // xcap의 macOS 영역 API는 포인트 단위이고, 전체 캡처 이미지는
        // Retina 픽셀 단위이므로 선택 좌표를 디스플레이 배율로 되돌린다.
        let scale = screen
            .scale_factor()
            .context("macOS 모니터 배율을 읽지 못했습니다")?
            .max(1.0);
        let logical_x = ((region_x as f32 / scale).floor() as i32).max(0) as u32;
        let logical_y = ((region_y as f32 / scale).floor() as i32).max(0) as u32;
        let logical_width = ((width as f32 / scale).ceil() as u32).max(1);
        let logical_height = ((height as f32 / scale).ceil() as u32).max(1);
        let source_rect = CGRect::new(
            logical_x as f64,
            logical_y as f64,
            logical_width as f64,
            logical_height as f64,
        );
        let screen_id = screen.id().context("macOS 모니터 식별자를 읽지 못했습니다")?;
        match capture_with_screen_capture_kit(screen_id, width, height, Some(source_rect)) {
            Ok(image) => Ok(image),
            Err(error) if screen_capture_kit_is_unavailable(&error) => screen
                .capture_region(logical_x, logical_y, logical_width, logical_height)
                .context("선택 영역 캡처에 실패했습니다"),
            Err(error) => Err(error),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    bail!("화면 캡처는 현재 Windows와 macOS에서 지원합니다")
}
