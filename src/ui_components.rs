use std::fs;

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2,
};

use super::openai;

pub(super) fn configure_korean_font(ctx: &egui::Context) {
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\Windows\Fonts\malgun.ttf",
            r"C:\Windows\Fonts\malgunsl.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        ]
    };
    let Some(bytes) = candidates.iter().find_map(|path| fs::read(path).ok()) else {
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("malgun".to_owned(), FontData::from_owned(bytes).into());
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "malgun".to_owned());
    }
    ctx.set_fonts(fonts);
}

pub(super) fn configure_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(12, 16, 24);
    visuals.window_fill = Color32::from_rgb(17, 22, 32);
    visuals.extreme_bg_color = Color32::from_rgb(10, 13, 20);
    visuals.faint_bg_color = Color32::from_rgb(24, 30, 42);
    visuals.selection.bg_fill = Color32::from_rgb(48, 137, 192);
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    ctx.set_style_of(egui::Theme::Dark, style);
}

pub(super) fn action_card(
    ui: &mut egui::Ui,
    title: &str,
    description: &str,
    hotkey: &str,
    accent: Color32,
) -> egui::Response {
    let desired_size = Vec2::new(ui.available_width(), 88.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if response.is_pointer_button_down_on() {
            Color32::from_rgb(30, 39, 54)
        } else if response.hovered() {
            Color32::from_rgb(27, 35, 49)
        } else {
            Color32::from_rgb(20, 27, 38)
        };
        let border = if response.hovered() {
            accent.gamma_multiply(0.8)
        } else {
            Color32::from_rgb(40, 51, 69)
        };
        ui.painter().rect(
            rect,
            12.0,
            fill,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        ui.painter().rect_filled(
            Rect::from_min_max(
                rect.min + Vec2::new(0.0, 12.0),
                Pos2::new(rect.min.x + 4.0, rect.max.y - 12.0),
            ),
            2.0,
            accent,
        );
        ui.painter().text(
            rect.min + Vec2::new(18.0, 17.0),
            egui::Align2::LEFT_TOP,
            title,
            FontId::proportional(17.0),
            Color32::from_rgb(239, 244, 252),
        );
        ui.painter().text(
            rect.min + Vec2::new(18.0, 53.0),
            egui::Align2::LEFT_TOP,
            description,
            FontId::proportional(12.0),
            Color32::from_rgb(136, 151, 174),
        );

        let hotkey_width = hotkey.chars().count() as f32 * 7.0 + 22.0;
        let hotkey_rect = Rect::from_min_size(
            Pos2::new(rect.max.x - hotkey_width - 14.0, rect.min.y + 13.0),
            Vec2::new(hotkey_width, 27.0),
        );
        ui.painter()
            .rect_filled(hotkey_rect, 7.0, Color32::from_rgb(34, 43, 58));
        ui.painter().text(
            hotkey_rect.center(),
            egui::Align2::CENTER_CENTER,
            hotkey,
            FontId::monospace(11.0),
            accent,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(super) fn friendly_error_message(error: &str) -> &str {
    if error.contains("응답하지 않았") || error.contains("제한 시간") {
        "번역 응답이 늦어 중단했습니다. 문장을 줄이거나 다시 시도해 주세요."
    } else if error.contains("선택한 텍스트") || error.contains("선택문") {
        "선택한 문장을 읽지 못했습니다. 문장을 선택한 뒤 다시 눌러 주세요."
    } else if error.contains("OCR") {
        "화면 글자를 읽지 못했습니다. 번역 영역을 다시 선택해 주세요."
    } else if error.contains("Claude Code를 찾지 못했습니다") {
        "Claude Code가 필요합니다. 공식 Claude Code를 설치한 뒤 다시 연결해 주세요."
    } else if error.contains("Claude") {
        "Claude 연결에 문제가 생겼습니다. 로그인 상태를 확인하고 다시 시도해 주세요."
    } else if error.contains("Codex App Server") || error.contains("번역 연결") {
        "ChatGPT 연결에 문제가 생겼습니다. 잠시 후 다시 시도해 주세요."
    } else {
        error
    }
}

pub(super) fn model_display_name(models: &[openai::ModelInfo], selected: &str) -> String {
    models
        .iter()
        .find(|model| model.id == selected)
        .map(|model| model.display_name.clone())
        .unwrap_or_else(|| selected.to_owned())
}

pub(super) fn reasoning_display_name(effort: &str) -> String {
    let korean = match effort {
        "none" => "속도 우선",
        "minimal" => "매우 빠름",
        "low" => "균형",
        "medium" => "문맥 강화",
        "high" => "정확도 우선",
        "xhigh" => "고정밀",
        "max" => "최대 정밀",
        "ultra" => "울트라 정밀",
        _ => return effort.to_owned(),
    };
    korean.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_extended_reasoning_efforts() {
        assert_eq!(reasoning_display_name("none"), "속도 우선");
        assert_eq!(reasoning_display_name("max"), "최대 정밀");
        assert_eq!(reasoning_display_name("ultra"), "울트라 정밀");
    }

    #[test]
    fn turns_internal_timeout_into_actionable_message() {
        assert_eq!(
            friendly_error_message("Codex App Server가 44초 동안 응답하지 않았습니다"),
            "번역 응답이 늦어 중단했습니다. 문장을 줄이거나 다시 시도해 주세요."
        );
    }
}
