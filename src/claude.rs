use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::openai::{ModelInfo, Translation, TranslationBatch, TranslationDirection};

pub const DEFAULT_MODEL: &str = "haiku";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const TRANSLATION_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(80);
const TRANSLATOR_INSTRUCTIONS: &str = "You are a fast English-Korean translator. Translate only in the requested direction. Preserve meaning, tone, names, numbers, code, URLs, emoji, and item count. Fix only unambiguous OCR mistakes. Never use tools, explain, summarize, or add content. Return only the requested JSON.";

#[derive(Deserialize)]
struct StructuredTranslation {
    translation: String,
}

#[derive(Deserialize)]
struct StructuredTranslations {
    translations: Vec<String>,
}

pub struct ClaudeClient {
    launcher: PathBuf,
    plan_type: String,
    model: String,
    reasoning_effort: String,
    available_models: Vec<ModelInfo>,
}

impl ClaudeClient {
    pub fn connect(
        requested_model: &str,
        requested_reasoning_effort: Option<&str>,
        mut status: impl FnMut(&str),
    ) -> Result<Self> {
        status("Claude 실행 파일을 확인하는 중…");
        let launcher = find_claude_launcher()?;

        status("Claude 계정 로그인을 확인하는 중…");
        let account = read_auth_status(&launcher).ok();
        if account.is_none() {
            status("브라우저에서 Claude 계정 로그인을 완료하세요…");
            login(&launcher)?;
        }
        let account = read_auth_status(&launcher).context("Claude 계정 로그인이 필요합니다")?;
        let plan_type = account_label(&account);

        let available_models = available_models();
        let selected = available_models
            .iter()
            .find(|model| model.id == requested_model)
            .or_else(|| {
                available_models
                    .iter()
                    .find(|model| model.id == DEFAULT_MODEL)
            })
            .context("사용할 수 있는 Claude 모델이 없습니다")?;
        let reasoning_effort = requested_reasoning_effort
            .filter(|effort| {
                selected
                    .reasoning_efforts
                    .iter()
                    .any(|value| value == effort)
            })
            .map(str::to_owned)
            .or_else(|| selected.reasoning_efforts.first().cloned())
            .unwrap_or_else(|| "none".to_owned());

        Ok(Self {
            launcher,
            plan_type,
            model: selected.id.clone(),
            reasoning_effort,
            available_models,
        })
    }

    pub fn plan_type(&self) -> &str {
        &self.plan_type
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn reasoning_effort(&self) -> &str {
        &self.reasoning_effort
    }

    pub fn available_models(&self) -> &[ModelInfo] {
        &self.available_models
    }

    pub fn translate(
        &mut self,
        source: &str,
        direction: TranslationDirection,
    ) -> Result<Translation> {
        self.translate_internal(source, direction, None)?
            .context("번역 작업이 취소되었습니다")
    }

    pub fn translate_cancellable(
        &mut self,
        source: &str,
        direction: TranslationDirection,
        cancelled: &AtomicBool,
    ) -> Result<Option<Translation>> {
        self.translate_internal(source, direction, Some(cancelled))
    }

    fn translate_internal(
        &mut self,
        source: &str,
        direction: TranslationDirection,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<Translation>> {
        if source.trim().is_empty() {
            bail!("번역할 내용이 비어 있습니다");
        }
        let started = Instant::now();
        let prompt = single_prompt(direction, source);
        let Some(output) = run_prompt(
            &self.launcher,
            &self.model,
            &self.reasoning_effort,
            &prompt,
            cancelled,
        )?
        else {
            return Ok(None);
        };
        let text = parse_translation(&extract_result(&output)?)?;
        if text.trim().is_empty() {
            bail!("{} 모델이 빈 번역을 반환했습니다", self.model);
        }
        Ok(Some(Translation {
            text,
            wall_time: started.elapsed(),
        }))
    }

    pub fn translate_batch_cancellable(
        &mut self,
        sources: &[String],
        context_lines: &[String],
        direction: TranslationDirection,
        cancelled: &AtomicBool,
    ) -> Result<Option<TranslationBatch>> {
        if sources.is_empty() {
            return Ok(Some(TranslationBatch {
                texts: Vec::new(),
                wall_time: Duration::ZERO,
            }));
        }
        let started = Instant::now();
        let prompt = batch_prompt(direction, sources, context_lines)?;
        let Some(output) = run_prompt(
            &self.launcher,
            &self.model,
            &self.reasoning_effort,
            &prompt,
            Some(cancelled),
        )?
        else {
            return Ok(None);
        };
        let texts = parse_translations(&extract_result(&output)?)?;
        if texts.len() != sources.len() {
            bail!(
                "{} 모델의 번역 개수가 맞지 않습니다 (요청 {}, 응답 {})",
                self.model,
                sources.len(),
                texts.len()
            );
        }
        Ok(Some(TranslationBatch {
            texts,
            wall_time: started.elapsed(),
        }))
    }
}

fn available_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo::new("haiku", "Claude Haiku · 최고 속도", vec!["none"], true),
        ModelInfo::new(
            "sonnet",
            "Claude Sonnet · 균형",
            vec!["low", "medium", "high"],
            false,
        ),
        ModelInfo::new(
            "opus",
            "Claude Opus · 최고 품질",
            vec!["low", "medium", "high", "xhigh", "max"],
            false,
        ),
    ]
}

fn single_prompt(direction: TranslationDirection, source: &str) -> String {
    let task = match direction {
        TranslationDirection::EnglishToKorean => {
            "다음 영어 OCR 원문을 자연스러운 한국어로 완전하게 번역하세요."
        }
        TranslationDirection::KoreanToEnglish => {
            "다음 한국어 원문을 자연스러운 영어로 완전하게 번역하세요."
        }
    };
    format!(
        "{task} JSON의 translation 필드에 번역문만 넣으세요.\n\n<source>\n{}\n</source>",
        source.trim()
    )
}

fn batch_prompt(
    direction: TranslationDirection,
    sources: &[String],
    context_lines: &[String],
) -> Result<String> {
    let task = match direction {
        TranslationDirection::EnglishToKorean => "각 영어 OCR 줄을 자연스러운 한국어로 번역하세요.",
        TranslationDirection::KoreanToEnglish => "각 한국어 줄을 자연스러운 영어로 번역하세요.",
    };
    Ok(format!(
        "{task} context_lines는 위에서 아래 순서의 문맥입니다. targets 순서와 개수를 유지하고 줄을 합치거나 나누지 마세요. JSON의 translations 배열에 대응 번역문만 넣으세요.\n{}",
        serde_json::to_string(&json!({
            "context_lines": context_lines,
            "targets": sources
        }))
        .context("Claude 번역 묶음 직렬화 실패")?
    ))
}

fn run_prompt(
    launcher: &Path,
    model: &str,
    reasoning_effort: &str,
    prompt: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<Option<String>> {
    if is_cancelled(cancelled) {
        return Ok(None);
    }

    let mut command = Command::new(launcher);
    command.args([
        "-p",
        "--output-format",
        "json",
        "--model",
        model,
        "--max-turns",
        "1",
        "--permission-mode",
        "dontAsk",
        "--no-session-persistence",
        "--disallowed-tools",
        "*",
        "--system-prompt",
        TRANSLATOR_INSTRUCTIONS,
    ]);
    if reasoning_effort != "none" {
        command.args(["--effort", reasoning_effort]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| format!("Claude 실행 실패: {}", launcher.display()))?;
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(prompt.as_bytes())
            .context("Claude 번역 요청 쓰기 실패")?;
    }

    let deadline = Instant::now() + TRANSLATION_TIMEOUT;
    loop {
        if is_cancelled(cancelled) {
            stop_child(&mut child);
            return Ok(None);
        }
        if child
            .try_wait()
            .context("Claude 실행 상태 확인 실패")?
            .is_some()
        {
            let output = child
                .wait_with_output()
                .context("Claude 번역 결과 읽기 실패")?;
            if !output.status.success() {
                let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                bail!(
                    "Claude 번역 실패: {}",
                    if message.is_empty() {
                        format!("종료 코드 {:?}", output.status.code())
                    } else {
                        message
                    }
                );
            }
            return Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()));
        }
        if Instant::now() >= deadline {
            stop_child(&mut child);
            bail!("Claude 번역 제한 시간(60초)을 초과했습니다");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn read_auth_status(launcher: &Path) -> Result<Value> {
    let mut command = Command::new(launcher);
    command.args(["auth", "status"]);
    hide_console(&mut command);
    let output = command.output().context("Claude 로그인 상태 확인 실패")?;
    if !output.status.success() {
        bail!("Claude 계정에 로그인되어 있지 않습니다");
    }
    serde_json::from_slice(&output.stdout).or_else(|_| {
        Ok(json!({
            "authenticated": true,
            "description": String::from_utf8_lossy(&output.stdout).trim()
        }))
    })
}

fn login(launcher: &Path) -> Result<()> {
    let mut command = Command::new(launcher);
    command
        .args(["auth", "login"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let mut child = command.spawn().context("Claude 로그인 시작 실패")?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().context("Claude 로그인 상태 확인 실패")? {
            if status.success() {
                return Ok(());
            }
            bail!("Claude 계정 로그인에 실패했습니다");
        }
        if Instant::now() >= deadline {
            stop_child(&mut child);
            bail!("Claude 로그인 대기 시간이 초과되었습니다. 다시 연결해 주세요");
        }
        thread::sleep(Duration::from_millis(150));
    }
}

fn account_label(account: &Value) -> String {
    for pointer in [
        "/subscriptionType",
        "/subscription_type",
        "/accountType",
        "/account_type",
        "/authMethod",
        "/auth_method",
    ] {
        if let Some(value) = account.pointer(pointer).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return title_case(value);
            }
        }
    }
    "구독".to_owned()
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn find_claude_launcher() -> Result<PathBuf> {
    if let Some(path) = env::var_os("DOG_TRANSLATE_CLAUDE_PATH")
        .or_else(|| env::var_os("DOF_TRANSLATE_CLAUDE_PATH"))
    {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    if let Ok(executable) = env::current_exe()
        && let Some(directory) = executable.parent()
    {
        for relative in claude_sidecar_names() {
            let candidate = directory.join(relative);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    if let Some(profile) = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let profile = PathBuf::from(profile);
        for relative in claude_user_install_names() {
            let candidate = profile.join(relative);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    find_on_path(if cfg!(windows) { "claude.exe" } else { "claude" }).ok_or_else(|| {
        anyhow!(
            "Claude Code를 찾지 못했습니다. 공식 Claude Code를 설치한 뒤 다시 연결하거나 DOG_TRANSLATE_CLAUDE_PATH를 설정하세요"
        )
    })
}

fn claude_sidecar_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["claude.exe", "providers/claude.exe", "runtime/claude.exe"]
    } else {
        &["claude", "providers/claude", "runtime/claude"]
    }
}

fn claude_user_install_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &[".local/bin/claude.exe", ".local/share/claude/claude.exe"]
    } else {
        &[".local/bin/claude", ".local/share/claude/claude"]
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("where.exe");
        command.arg(name);
        command
    } else {
        let mut command = Command::new("which");
        command.arg(name);
        command
    };
    hide_console(&mut command);
    let output = command.output().ok()?;
    output.status.success().then_some(())?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

fn extract_result(output: &str) -> Result<String> {
    let trimmed = output.trim();
    let value: Value = serde_json::from_str(trimmed).with_context(|| {
        format!(
            "Claude JSON 응답을 읽지 못했습니다: {}",
            abbreviate(trimmed)
        )
    })?;
    if let Some(result) = value.get("result").and_then(Value::as_str) {
        return Ok(result.to_owned());
    }
    if let Some(result) = value
        .pointer("/message/content/0/text")
        .and_then(Value::as_str)
    {
        return Ok(result.to_owned());
    }
    bail!("Claude 응답에 번역 결과가 없습니다")
}

fn parse_translation(message: &str) -> Result<String> {
    let parsed = parse_json_body::<StructuredTranslation>(message)?;
    Ok(parsed.translation.trim().to_owned())
}

fn parse_translations(message: &str) -> Result<Vec<String>> {
    let parsed = parse_json_body::<StructuredTranslations>(message)?;
    Ok(parsed
        .translations
        .into_iter()
        .map(|translation| translation.trim().to_owned())
        .collect())
}

fn parse_json_body<T: for<'de> Deserialize<'de>>(message: &str) -> Result<T> {
    let trimmed = message.trim();
    if let Ok(parsed) = serde_json::from_str(trimmed) {
        return Ok(parsed);
    }
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    serde_json::from_str(without_fence).with_context(|| {
        format!(
            "Claude 번역 JSON을 읽지 못했습니다: {}",
            abbreviate(trimmed)
        )
    })
}

fn abbreviate(value: &str) -> String {
    const LIMIT: usize = 180;
    let mut shortened = value.chars().take(LIMIT).collect::<String>();
    if value.chars().count() > LIMIT {
        shortened.push('…');
    }
    shortened
}

fn is_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_result_envelope() {
        let output = r#"{"result":"{\"translation\":\"안녕하세요\"}"}"#;
        assert_eq!(
            extract_result(output).unwrap(),
            r#"{"translation":"안녕하세요"}"#
        );
    }

    #[test]
    fn parses_fenced_translation_array() {
        let parsed =
            parse_translations("```json\n{\"translations\":[\"하나\",\"둘\"]}\n```").unwrap();
        assert_eq!(parsed, vec!["하나", "둘"]);
    }

    #[test]
    fn exposes_fast_balanced_and_quality_models() {
        let models = available_models();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["haiku", "sonnet", "opus"]
        );
    }
}
