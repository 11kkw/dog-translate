#[cfg(not(target_os = "windows"))]
use anyhow::bail;
use anyhow::{Context, Result};
use xcap::{Monitor, image::RgbaImage};

#[cfg(target_os = "windows")]
use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};

#[cfg(target_os = "macos")]
use std::{ffi::c_void, ptr};

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

pub struct CapturedScreen {
    pub image: RgbaImage,
    pub x: i32,
    pub y: i32,
    pub scale_factor: f32,
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
        screen
            .capture_region(logical_x, logical_y, logical_width, logical_height)
            .context("선택 영역 캡처에 실패했습니다")
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    bail!("화면 캡처는 현재 Windows와 macOS에서 지원합니다")
}
