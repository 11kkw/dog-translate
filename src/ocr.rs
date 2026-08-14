use anyhow::{Context, Result, bail};
#[cfg(target_os = "windows")]
use futures::executor::block_on;
use xcap::image::{Rgba, RgbaImage, imageops};

#[cfg(target_os = "macos")]
use std::io::Cursor;

#[cfg(target_os = "macos")]
use anyhow::anyhow;
#[cfg(target_os = "macos")]
use objc2::{AnyThread, runtime::AnyObject};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSData, NSDictionary, ns_string};
#[cfg(target_os = "macos")]
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};
#[cfg(target_os = "macos")]
use xcap::image::{DynamicImage, ImageFormat};

#[cfg(target_os = "windows")]
use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::{OcrEngine, OcrResult},
    Security::Cryptography::CryptographicBuffer,
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::HSTRING,
};

#[derive(Debug, Clone)]
pub struct RecognizedLine {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone)]
#[cfg(any(target_os = "windows", test))]
struct WordBox {
    text: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

pub fn recognize_english(image: &RgbaImage) -> Result<String> {
    let lines = recognize_english_lines(image)?;
    if lines.is_empty() {
        bail!("선택한 영역에서 영어 문자를 찾지 못했습니다");
    }
    Ok(lines
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(target_os = "windows")]
pub fn recognize_english_lines(image: &RgbaImage) -> Result<Vec<RecognizedLine>> {
    let _apartment = WinRtApartment::initialize()?;
    if image.width() == 0 || image.height() == 0 {
        bail!("OCR 화면의 크기가 0입니다");
    }

    let engine = create_ocr_engine()?;
    let prepared = prepare_capture(image);
    let scale_x = image.width() as f32 / prepared.width() as f32;
    let scale_y = image.height() as f32 / prepared.height() as f32;
    let baseline = block_on(recognize_bitmap(&prepared, &engine))
        .and_then(|result| extract_recognized_lines(&result, scale_x, scale_y));

    let enhanced = enhance_for_ocr(&prepared);
    let enhanced = block_on(recognize_bitmap(&enhanced, &engine))
        .and_then(|result| extract_recognized_lines(&result, scale_x, scale_y));

    match (baseline, enhanced) {
        (Ok(baseline), Ok(enhanced)) => Ok(select_and_complete_ocr_pass(baseline, enhanced)),
        (Ok(baseline), Err(_)) => Ok(baseline),
        (Err(_), Ok(enhanced)) => Ok(enhanced),
        (Err(baseline_error), Err(enhanced_error)) => Err(baseline_error)
            .with_context(|| format!("OCR 기본·보정 인식이 모두 실패했습니다: {enhanced_error:#}")),
    }
}

#[cfg(target_os = "macos")]
pub fn recognize_english_lines(image: &RgbaImage) -> Result<Vec<RecognizedLine>> {
    if image.width() == 0 || image.height() == 0 {
        bail!("OCR 화면의 크기가 0입니다");
    }

    let prepared = prepare_capture(image);
    let scale_x = image.width() as f32 / prepared.width() as f32;
    let scale_y = image.height() as f32 / prepared.height() as f32;
    let baseline =
        recognize_vision(&prepared).map(|lines| scale_recognized_lines(lines, scale_x, scale_y));

    let enhanced_image = enhance_for_ocr(&prepared);
    let enhanced = recognize_vision(&enhanced_image)
        .map(|lines| scale_recognized_lines(lines, scale_x, scale_y));

    match (baseline, enhanced) {
        (Ok(baseline), Ok(enhanced)) => Ok(select_and_complete_ocr_pass(baseline, enhanced)),
        (Ok(baseline), Err(_)) => Ok(baseline),
        (Err(_), Ok(enhanced)) => Ok(enhanced),
        (Err(baseline_error), Err(enhanced_error)) => Err(baseline_error)
            .with_context(|| format!("OCR 기본·보정 인식이 모두 실패했습니다: {enhanced_error:#}")),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn recognize_english_lines(_image: &RgbaImage) -> Result<Vec<RecognizedLine>> {
    bail!("운영체제 OCR은 현재 Windows와 macOS에서 지원합니다")
}

#[cfg(target_os = "windows")]
fn extract_recognized_lines(
    result: &OcrResult,
    scale_x: f32,
    scale_y: f32,
) -> Result<Vec<RecognizedLine>> {
    let lines = result.Lines().context("OCR 줄 목록 읽기 실패")?;
    let mut recognized = Vec::with_capacity(lines.Size().unwrap_or(0) as usize);

    for index in 0..lines.Size().context("OCR 줄 수 읽기 실패")? {
        let line = lines.GetAt(index).context("OCR 줄 읽기 실패")?;
        let words = line.Words().context("OCR 단어 목록 읽기 실패")?;
        let mut word_boxes = Vec::with_capacity(words.Size().unwrap_or(0) as usize);
        for word_index in 0..words.Size().context("OCR 단어 수 읽기 실패")? {
            let word = words.GetAt(word_index).context("OCR 단어 읽기 실패")?;
            let text = normalize_text(
                &word
                    .Text()
                    .context("OCR 단어 텍스트 읽기 실패")?
                    .to_string(),
            );
            let rect = word.BoundingRect().context("OCR 단어 위치 읽기 실패")?;
            if !text.is_empty() {
                word_boxes.push(WordBox {
                    text,
                    x: rect.X * scale_x,
                    y: rect.Y * scale_y,
                    width: rect.Width * scale_x,
                    height: rect.Height * scale_y,
                });
            }
        }
        recognized.extend(split_word_runs(&word_boxes));
    }

    recognized.sort_by(|left, right| {
        left.y
            .total_cmp(&right.y)
            .then_with(|| left.x.total_cmp(&right.x))
    });

    Ok(recognized)
}

#[cfg(target_os = "macos")]
fn recognize_vision(image: &RgbaImage) -> Result<Vec<RecognizedLine>> {
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut encoded, ImageFormat::Png)
        .context("macOS Vision OCR 이미지 준비 실패")?;

    let image_data = NSData::with_bytes(encoded.get_ref());
    let options = NSDictionary::<VNImageOption, AnyObject>::new();
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &image_data,
        &options,
    );
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    let languages = NSArray::from_slice(&[ns_string!("en-US")]);
    request.setRecognitionLanguages(&languages);

    let request_ref: &VNRequest = request.as_ref();
    let requests = NSArray::from_slice(&[request_ref]);
    handler
        .performRequests_error(&requests)
        .map_err(|error| anyhow!("macOS Vision OCR 실행 실패: {error:?}"))?;

    let observations = request
        .results()
        .ok_or_else(|| anyhow!("macOS Vision OCR 결과가 없습니다"))?;
    let image_width = image.width() as f64;
    let image_height = image.height() as f64;
    let mut recognized = Vec::with_capacity(observations.len());

    for observation in observations.to_vec() {
        let Some(candidate) = observation.topCandidates(1).firstObject() else {
            continue;
        };
        let text = normalize_text(&candidate.string().to_string());
        if !looks_like_english(&text) {
            continue;
        }

        let bounds = unsafe { observation.boundingBox() };
        let x = (bounds.origin.x * image_width) as f32;
        let y = ((1.0 - bounds.origin.y - bounds.size.height) * image_height) as f32;
        let width = (bounds.size.width * image_width) as f32;
        let height = (bounds.size.height * image_height) as f32;
        recognized.push(RecognizedLine {
            text,
            x: x.max(0.0),
            y: y.max(0.0),
            width: width.max(1.0),
            height: height.max(1.0),
        });
    }

    recognized.sort_by(|left, right| {
        left.y
            .total_cmp(&right.y)
            .then_with(|| left.x.total_cmp(&right.x))
    });
    Ok(recognized)
}

#[cfg(target_os = "macos")]
fn scale_recognized_lines(
    mut lines: Vec<RecognizedLine>,
    scale_x: f32,
    scale_y: f32,
) -> Vec<RecognizedLine> {
    for line in &mut lines {
        line.x *= scale_x;
        line.y *= scale_y;
        line.width *= scale_x;
        line.height *= scale_y;
    }
    lines
}

#[cfg(any(target_os = "windows", test))]
fn split_word_runs(words: &[WordBox]) -> Vec<RecognizedLine> {
    if words.is_empty() {
        return Vec::new();
    }

    let mut ordered = words.to_vec();
    ordered.sort_by(|left, right| {
        left.x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y))
    });
    let mut heights = ordered.iter().map(|word| word.height).collect::<Vec<_>>();
    heights.sort_by(f32::total_cmp);
    let median_height = heights[heights.len() / 2].max(1.0);
    let horizontal_gap_limit = (median_height * 1.8).clamp(18.0, 64.0);
    let vertical_center_limit = (median_height * 0.8).max(8.0);

    let mut runs = Vec::new();
    let mut run_start = 0;
    for index in 1..ordered.len() {
        let previous = &ordered[index - 1];
        let current = &ordered[index];
        let gap = current.x - (previous.x + previous.width);
        let previous_center = previous.y + previous.height * 0.5;
        let current_center = current.y + current.height * 0.5;
        if gap > horizontal_gap_limit
            || (current_center - previous_center).abs() > vertical_center_limit
        {
            if let Some(line) = line_from_words(&ordered[run_start..index]) {
                runs.push(line);
            }
            run_start = index;
        }
    }
    if let Some(line) = line_from_words(&ordered[run_start..]) {
        runs.push(line);
    }
    runs
}

#[cfg(any(target_os = "windows", test))]
fn line_from_words(words: &[WordBox]) -> Option<RecognizedLine> {
    let text = normalize_text(
        &words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    if !looks_like_english(&text) {
        return None;
    }

    let left = words
        .iter()
        .map(|word| word.x)
        .fold(f32::INFINITY, f32::min);
    let top = words
        .iter()
        .map(|word| word.y)
        .fold(f32::INFINITY, f32::min);
    let right = words
        .iter()
        .map(|word| word.x + word.width)
        .fold(f32::NEG_INFINITY, f32::max);
    let bottom = words
        .iter()
        .map(|word| word.y + word.height)
        .fold(f32::NEG_INFINITY, f32::max);
    Some(RecognizedLine {
        text,
        x: left,
        y: top,
        width: (right - left).max(1.0),
        height: (bottom - top).max(1.0),
    })
}

#[cfg(target_os = "windows")]
struct WinRtApartment;

#[cfg(target_os = "windows")]
impl WinRtApartment {
    fn initialize() -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .context("Windows Runtime OCR 초기화 실패")?;
        Ok(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for WinRtApartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

fn prepare_capture(image: &RgbaImage) -> RgbaImage {
    let longest = image.width().max(image.height());
    #[cfg(target_os = "windows")]
    let max_dimension = OcrEngine::MaxImageDimension().unwrap_or(2_600).max(1);
    #[cfg(target_os = "macos")]
    let max_dimension = 4_096;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let max_dimension = 2_600;
    let target_longest = if longest < 1_600 {
        longest.saturating_mul(2).min(2_400).min(max_dimension)
    } else {
        longest.min(max_dimension)
    };

    if target_longest == longest {
        return image.clone();
    }

    let scale = target_longest as f64 / longest as f64;
    let width = ((image.width() as f64 * scale).round() as u32).clamp(1, max_dimension);
    let height = ((image.height() as f64 * scale).round() as u32).clamp(1, max_dimension);

    imageops::resize(image, width, height, imageops::FilterType::CatmullRom)
}

fn enhance_for_ocr(image: &RgbaImage) -> RgbaImage {
    let mut histogram = [0_u32; 256];
    let mut luminance = Vec::with_capacity((image.width() * image.height()) as usize);
    for pixel in image.pixels() {
        let value = pixel_luminance(pixel);
        histogram[value as usize] += 1;
        luminance.push(value);
    }

    let total = luminance.len() as u32;
    let low = histogram_percentile(&histogram, total, 2);
    let high = histogram_percentile(&histogram, total, 98);
    let median = histogram_percentile(&histogram, total, 50);
    let range = high.saturating_sub(low).max(24) as f32;
    let invert = median < 128;

    let mut contrasted = RgbaImage::new(image.width(), image.height());
    for (pixel, &value) in contrasted.pixels_mut().zip(&luminance) {
        let stretched = (((value.saturating_sub(low)) as f32 / range) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
        let normalized = if invert { 255 - stretched } else { stretched };
        *pixel = Rgba([normalized, normalized, normalized, 255]);
    }

    let blurred = imageops::blur(&contrasted, 0.8);
    for (pixel, blurred_pixel) in contrasted.pixels_mut().zip(blurred.pixels()) {
        let original = pixel[0] as f32;
        let blurred = blurred_pixel[0] as f32;
        let sharpened = (original + (original - blurred) * 0.65)
            .round()
            .clamp(0.0, 255.0) as u8;
        *pixel = Rgba([sharpened, sharpened, sharpened, 255]);
    }
    contrasted
}

fn pixel_luminance(pixel: &Rgba<u8>) -> u8 {
    ((pixel[0] as u32 * 54 + pixel[1] as u32 * 183 + pixel[2] as u32 * 19) >> 8) as u8
}

fn histogram_percentile(histogram: &[u32; 256], total: u32, percentile: u32) -> u8 {
    if total == 0 {
        return 0;
    }
    let target = ((total as u64 * percentile as u64) / 100).max(1) as u32;
    let mut seen = 0_u32;
    for (value, count) in histogram.iter().enumerate() {
        seen = seen.saturating_add(*count);
        if seen >= target {
            return value as u8;
        }
    }
    255
}

fn select_and_complete_ocr_pass(
    baseline: Vec<RecognizedLine>,
    enhanced: Vec<RecognizedLine>,
) -> Vec<RecognizedLine> {
    let (mut selected, secondary) = if ocr_pass_quality(&enhanced) > ocr_pass_quality(&baseline) {
        (enhanced, baseline)
    } else {
        (baseline, enhanced)
    };

    for candidate in secondary {
        if let Some(index) = selected
            .iter()
            .position(|existing| same_ocr_region(existing, &candidate))
        {
            if line_text_quality(&candidate.text) > line_text_quality(&selected[index].text) {
                selected[index] = candidate;
            }
        } else {
            selected.push(candidate);
        }
    }
    selected.sort_by(|left, right| {
        left.y
            .total_cmp(&right.y)
            .then_with(|| left.x.total_cmp(&right.x))
    });
    selected
}

fn ocr_pass_quality(lines: &[RecognizedLine]) -> usize {
    lines
        .iter()
        .map(|line| line_text_quality(&line.text) + 4)
        .sum()
}

fn line_text_quality(text: &str) -> usize {
    let latin = text
        .chars()
        .filter(|character| character.is_ascii_alphabetic())
        .count();
    let digits = text
        .chars()
        .filter(|character| character.is_ascii_digit())
        .count();
    let words = text.split_whitespace().count();
    latin * 5 + digits * 2 + words * 3
}

fn same_ocr_region(left: &RecognizedLine, right: &RecognizedLine) -> bool {
    let line_height = left.height.max(right.height).max(1.0);
    let vertical_distance = (left.y + left.height * 0.5 - right.y - right.height * 0.5).abs();
    if vertical_distance > line_height * 0.85 {
        return false;
    }
    let left_end = left.x + left.width;
    let right_end = right.x + right.width;
    let horizontal_gap = if left_end < right.x {
        right.x - left_end
    } else if right_end < left.x {
        left.x - right_end
    } else {
        0.0
    };
    horizontal_gap <= line_height * 2.0
}

#[cfg(target_os = "windows")]
fn create_ocr_engine() -> Result<OcrEngine> {
    Language::CreateLanguage(&HSTRING::from("en-US"))
        .and_then(|language| OcrEngine::TryCreateFromLanguage(&language))
        .or_else(|_| OcrEngine::TryCreateFromUserProfileLanguages())
        .context("사용 가능한 Windows OCR 언어 엔진이 없습니다")
}

#[cfg(target_os = "windows")]
async fn recognize_bitmap(image: &RgbaImage, engine: &OcrEngine) -> Result<OcrResult> {
    let mut bgra = Vec::with_capacity(image.as_raw().len());
    for rgba in image.as_raw().chunks_exact(4) {
        bgra.extend_from_slice(&[rgba[2], rgba[1], rgba[0], rgba[3]]);
    }

    let buffer =
        CryptographicBuffer::CreateFromByteArray(&bgra).context("OCR 픽셀 버퍼 생성 실패")?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        image.width() as i32,
        image.height() as i32,
        BitmapAlphaMode::Ignore,
    )
    .context("OCR 비트맵 생성 실패")?;

    let result = engine
        .RecognizeAsync(&bitmap)
        .context("Windows OCR 실행 실패")?
        .await
        .context("Windows OCR 결과 처리 실패")?;

    Ok(result)
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn looks_like_english(text: &str) -> bool {
    let latin = text.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    let alphanumeric = text.chars().filter(|ch| ch.is_alphanumeric()).count();
    latin >= 2 && latin * 2 >= alphanumeric.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, x: f32, width: f32) -> WordBox {
        WordBox {
            text: text.to_owned(),
            x,
            y: 10.0,
            width,
            height: 16.0,
        }
    }

    fn recognized(text: &str, x: f32, y: f32) -> RecognizedLine {
        RecognizedLine {
            text: text.to_owned(),
            x,
            y,
            width: 180.0,
            height: 18.0,
        }
    }

    #[test]
    fn keeps_normal_sentence_as_one_visual_run() {
        let runs = split_word_runs(&[
            word("Hello", 10.0, 38.0),
            word("from", 54.0, 31.0),
            word("Discord", 91.0, 52.0),
        ]);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "Hello from Discord");
    }

    #[test]
    fn splits_unrelated_columns_on_large_gap() {
        let runs = split_word_runs(&[
            word("User", 10.0, 38.0),
            word("Hello", 122.0, 38.0),
            word("there", 166.0, 36.0),
        ]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "User");
        assert_eq!(runs[1].text, "Hello there");
    }

    #[test]
    fn normalizes_dark_mode_for_ocr() {
        let mut image = RgbaImage::from_pixel(40, 20, Rgba([20, 20, 20, 255]));
        for y in 6..14 {
            for x in 8..32 {
                image.put_pixel(x, y, Rgba([235, 235, 235, 255]));
            }
        }

        let enhanced = enhance_for_ocr(&image);
        assert!(enhanced.get_pixel(0, 0)[0] > 220);
        assert!(enhanced.get_pixel(16, 10)[0] < 35);
    }

    #[test]
    fn prefers_the_more_complete_text_for_the_same_region() {
        let selected = select_and_complete_ocr_pass(
            vec![recognized("Helo", 20.0, 10.0)],
            vec![recognized("Hello from Discord", 20.0, 10.0)],
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].text, "Hello from Discord");
    }

    #[test]
    fn adds_a_line_missed_by_the_stronger_pass_without_duplicates() {
        let selected = select_and_complete_ocr_pass(
            vec![recognized("A much longer primary sentence", 20.0, 10.0)],
            vec![
                recognized("Primary sentence", 20.0, 10.0),
                recognized("OK", 20.0, 48.0),
            ],
        );

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].text, "A much longer primary sentence");
        assert_eq!(selected[1].text, "OK");
    }
}
