use std::{
    env,
    ffi::OsString,
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_MODEL: &str = "gpt-5.6-luna";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const THREAD_TURN_LIMIT: u32 = 48;
const SERVER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSLATION_TIMEOUT: Duration = Duration::from_secs(45);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const TRANSLATOR_INSTRUCTIONS: &str = "Translate only in the requested English-Korean direction. Write natural chat-ready text and preserve meaning, tone, names, numbers, code, and URLs. Fix only unambiguous OCR mistakes. Never use tools or add explanations. Return only the required JSON.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationDirection {
    EnglishToKorean,
    KoreanToEnglish,
}

impl TranslationDirection {
    fn prompt(self, source: &str) -> String {
        let task = match self {
            Self::EnglishToKorean => {
                "다음 OCR 영어 원문을 자연스러운 한국어로 번역하세요. 원문의 의미, 정보량, 말투, 고유명사, 숫자, 이모지를 빠짐없이 보존하고 요약·생략·해설·내용 추가를 하지 마세요. 명백한 OCR 오타만 문맥에 맞게 교정하세요."
            }
            Self::KoreanToEnglish => {
                "다음 한국어 원문을 자연스러운 영어로 번역하세요. 원문의 의미, 정보량, 말투, 고유명사, 숫자, 이모지를 빠짐없이 보존하고 요약·생략·해설·내용 추가를 하지 마세요. 채팅이나 입력창에 바로 붙여넣을 수 있게 번역문만 작성하세요."
            }
        };
        format!(
            "{task} JSON의 translation 필드에는 번역문만 넣으세요.\n\n<source>\n{}\n</source>",
            source.trim()
        )
    }

    fn batch_prompt(self, sources: &[String], context_lines: &[String]) -> Result<String> {
        let task = match self {
            Self::EnglishToKorean => {
                "각 영어 OCR 줄을 자연스러운 한국어로 번역하세요. 각 원문의 의미와 정보량을 빠짐없이 보존하고 요약·생략·내용 추가를 하지 마세요."
            }
            Self::KoreanToEnglish => {
                "각 한국어 줄을 자연스러운 영어로 번역하세요. 각 원문의 의미와 정보량을 빠짐없이 보존하고 요약·생략·내용 추가를 하지 마세요."
            }
        };
        Ok(format!(
            "{task} context_lines 전체를 위에서 아래 순서의 화면 문맥으로 참고하되, targets의 각 원문은 반드시 독립된 번역 한 개로 대응하세요. targets의 순서와 개수를 정확히 유지하고 서로 다른 줄을 합치거나 한 줄을 여러 항목으로 나누지 마세요. translations 배열에는 targets에 대응하는 번역문만 넣으세요.\n{}",
            serde_json::to_string(&json!({
                "context_lines": context_lines,
                "targets": sources
            }))
            .context("문맥 포함 번역 묶음 직렬화 실패")?
        ))
    }
}

pub struct OpenAiClient {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: Receiver<std::result::Result<Value, String>>,
    next_id: u64,
    plan_type: String,
    model: String,
    reasoning_effort: String,
    available_models: Vec<ModelInfo>,
    english_korean_thread: Option<TranslationThread>,
    korean_english_thread: Option<TranslationThread>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub reasoning_efforts: Vec<String>,
    is_default: bool,
}

impl ModelInfo {
    pub(crate) fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        reasoning_efforts: Vec<&str>,
        is_default: bool,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            reasoning_efforts: reasoning_efforts.into_iter().map(str::to_owned).collect(),
            is_default,
        }
    }
}

struct TranslationThread {
    id: String,
    turns: u32,
}

#[derive(Debug, Clone)]
pub struct Translation {
    pub text: String,
    pub wall_time: Duration,
}

#[derive(Deserialize)]
struct StructuredTranslation {
    translation: String,
}

#[derive(Deserialize)]
struct StructuredTranslations {
    translations: Vec<String>,
}

impl OpenAiClient {
    pub fn connect(
        requested_model: &str,
        requested_reasoning_effort: Option<&str>,
        mut status: impl FnMut(&str),
    ) -> Result<Self> {
        status("Codex App Server를 시작하는 중…");
        let mut child = spawn_app_server()?;
        let input = child
            .stdin
            .take()
            .context("Codex App Server 입력 스트림을 열지 못했습니다")?;
        let output = child
            .stdout
            .take()
            .context("Codex App Server 출력 스트림을 열지 못했습니다")?;
        let output = spawn_server_reader(output);

        let mut client = Self {
            child,
            input: BufWriter::new(input),
            output,
            next_id: 1,
            plan_type: String::new(),
            model: String::new(),
            reasoning_effort: "none".to_owned(),
            available_models: Vec::new(),
            english_korean_thread: None,
            korean_english_thread: None,
        };

        client.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "dog_translate",
                    "title": "Dog Translate",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "optOutNotificationMethods": [
                        "item/agentMessage/delta",
                        "item/reasoning/summaryTextDelta",
                        "item/reasoning/summaryPartAdded"
                    ]
                }
            }),
        )?;
        client.send(&json!({"method": "initialized", "params": {}}))?;

        status("ChatGPT 계정 로그인을 확인하는 중…");
        let account = client.read_account()?;
        if account
            .get("type")
            .and_then(Value::as_str)
            .is_none_or(|kind| kind != "chatgpt")
        {
            status("브라우저에서 ChatGPT 로그인을 완료하세요…");
            client.login_with_chatgpt()?;
        }

        let account = client.read_account()?;
        if account.get("type").and_then(Value::as_str) != Some("chatgpt") {
            bail!("ChatGPT 계정 로그인이 필요합니다");
        }
        client.plan_type = account
            .get("planType")
            .and_then(Value::as_str)
            .unwrap_or("구독")
            .to_owned();

        status("사용 가능한 번역 모델을 확인하는 중…");
        client.available_models = client.read_available_models()?;
        let selected = resolve_model(requested_model, &client.available_models)
            .context("현재 ChatGPT 계정에서 사용할 수 있는 모델이 없습니다")?;
        client.model = selected.id.clone();
        client.reasoning_effort = resolve_reasoning_effort(requested_reasoning_effort, selected);
        Ok(client)
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
        if is_cancelled(cancelled) {
            return Ok(None);
        }
        let started = Instant::now();
        let Some(thread_id) = self.translation_thread(direction, cancelled)? else {
            return Ok(None);
        };
        if is_cancelled(cancelled) {
            return Ok(None);
        }
        let prompt = direction.prompt(source);
        let Some(turn) = self.request_internal(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt}],
                "model": self.model,
                "effort": self.reasoning_effort,
                "summary": "none",
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "readOnly"},
                "outputSchema": {
                    "type": "object",
                    "properties": {
                        "translation": {"type": "string"}
                    },
                    "required": ["translation"],
                    "additionalProperties": false
                }
            }),
            cancelled,
        )?
        else {
            return Ok(None);
        };
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("Codex App Server가 번역 작업 ID를 반환하지 않았습니다")?
            .to_owned();

        let Some(text) = self.wait_for_translation(&turn_id, cancelled)? else {
            return Ok(None);
        };
        let text = parse_translation(&text)?;
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
        self.translate_batch_internal(sources, context_lines, direction, Some(cancelled))
    }

    fn translate_batch_internal(
        &mut self,
        sources: &[String],
        context_lines: &[String],
        direction: TranslationDirection,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<TranslationBatch>> {
        if sources.is_empty() {
            return Ok(Some(TranslationBatch {
                texts: Vec::new(),
                wall_time: Duration::ZERO,
            }));
        }
        if is_cancelled(cancelled) {
            return Ok(None);
        }

        let started = Instant::now();
        let Some(thread_id) = self.translation_thread(direction, cancelled)? else {
            return Ok(None);
        };
        if is_cancelled(cancelled) {
            return Ok(None);
        }
        let prompt = direction.batch_prompt(sources, context_lines)?;
        let Some(turn) = self.request_internal(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt}],
                "model": self.model,
                "effort": self.reasoning_effort,
                "summary": "none",
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "readOnly"},
                "outputSchema": {
                    "type": "object",
                    "properties": {
                        "translations": {
                            "type": "array",
                            "items": {"type": "string"},
                            "minItems": sources.len(),
                            "maxItems": sources.len()
                        }
                    },
                    "required": ["translations"],
                    "additionalProperties": false
                }
            }),
            cancelled,
        )?
        else {
            return Ok(None);
        };
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("Codex App Server가 일괄 번역 작업 ID를 반환하지 않았습니다")?
            .to_owned();
        let Some(message) = self.wait_for_translation(&turn_id, cancelled)? else {
            return Ok(None);
        };
        let texts = parse_translations(&message)?;
        if texts.len() != sources.len() {
            bail!(
                "{} 일괄 번역 개수가 맞지 않습니다 (요청 {}, 응답 {})",
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

    fn translation_thread(
        &mut self,
        direction: TranslationDirection,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<String>> {
        let needs_new = match direction {
            TranslationDirection::EnglishToKorean => self
                .english_korean_thread
                .as_ref()
                .is_none_or(|thread| thread.turns >= THREAD_TURN_LIMIT),
            TranslationDirection::KoreanToEnglish => self
                .korean_english_thread
                .as_ref()
                .is_none_or(|thread| thread.turns >= THREAD_TURN_LIMIT),
        };

        if needs_new {
            let Some(id) = self.start_translation_thread(cancelled)? else {
                return Ok(None);
            };
            let thread = Some(TranslationThread { id, turns: 0 });
            match direction {
                TranslationDirection::EnglishToKorean => self.english_korean_thread = thread,
                TranslationDirection::KoreanToEnglish => self.korean_english_thread = thread,
            }
        }

        let thread = match direction {
            TranslationDirection::EnglishToKorean => self.english_korean_thread.as_mut(),
            TranslationDirection::KoreanToEnglish => self.korean_english_thread.as_mut(),
        }
        .context("번역 스레드 초기화 실패")?;
        thread.turns += 1;
        Ok(Some(thread.id.clone()))
    }

    fn start_translation_thread(
        &mut self,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<String>> {
        let Some(thread) = self.request_internal(
            "thread/start",
            json!({
                "model": self.model,
                "cwd": env::current_dir().context("현재 작업 폴더를 확인하지 못했습니다")?,
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "ephemeral": true,
                "baseInstructions": TRANSLATOR_INSTRUCTIONS,
                "developerInstructions": TRANSLATOR_INSTRUCTIONS,
                "config": {"mcp_servers": {}},
                "serviceName": "dog_translate"
            }),
            cancelled,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(
            thread
                .pointer("/thread/id")
                .and_then(Value::as_str)
                .context("Codex App Server가 번역 스레드 ID를 반환하지 않았습니다")?
                .to_owned(),
        ))
    }

    fn read_account(&mut self) -> Result<Value> {
        let result = self.request(
            "account/read",
            json!({
                "refreshToken": false
            }),
        )?;
        Ok(result.get("account").cloned().unwrap_or(Value::Null))
    }

    fn login_with_chatgpt(&mut self) -> Result<()> {
        let result = self.request(
            "account/login/start",
            json!({
                "type": "chatgpt",
                "useHostedLoginSuccessPage": true,
                "appBrand": "chatgpt"
            }),
        )?;
        let login_id = result
            .get("loginId")
            .and_then(Value::as_str)
            .context("ChatGPT 로그인 ID를 받지 못했습니다")?
            .to_owned();
        let auth_url = result
            .get("authUrl")
            .and_then(Value::as_str)
            .context("ChatGPT 로그인 주소를 받지 못했습니다")?;
        open_browser(auth_url)?;

        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("ChatGPT 로그인 대기 시간이 초과되었습니다. 다시 연결해 주세요");
            }
            let message = self.read_message(remaining)?;
            if message.get("method").and_then(Value::as_str) != Some("account/login/completed") {
                self.answer_server_request_if_needed(&message)?;
                continue;
            }
            let params = message.get("params").unwrap_or(&Value::Null);
            if params.get("loginId").and_then(Value::as_str) != Some(&login_id) {
                continue;
            }
            if params.get("success").and_then(Value::as_bool) == Some(true) {
                return Ok(());
            }
            let error = params
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("알 수 없는 로그인 오류");
            bail!("ChatGPT 로그인 실패: {error}");
        }
    }

    fn read_available_models(&mut self) -> Result<Vec<ModelInfo>> {
        let result = self.request(
            "model/list",
            json!({
                "limit": 100,
                "includeHidden": false
            }),
        )?;
        let entries = result
            .get("data")
            .and_then(Value::as_array)
            .context("Codex App Server가 모델 목록을 반환하지 않았습니다")?;
        let mut models = Vec::new();
        for entry in entries {
            let Some(id) = entry
                .get("model")
                .or_else(|| entry.get("id"))
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
            else {
                continue;
            };
            if models.iter().any(|model: &ModelInfo| model.id == id) {
                continue;
            }
            let display_name = entry
                .get("displayName")
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(id)
                .to_owned();
            let reasoning_efforts = reasoning_efforts(entry, id);
            models.push(ModelInfo {
                id: id.to_owned(),
                display_name,
                reasoning_efforts,
                is_default: entry
                    .get("isDefault")
                    .or_else(|| entry.get("default"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
        if models.is_empty() {
            bail!("현재 ChatGPT 계정에서 사용할 수 있는 모델을 찾지 못했습니다");
        }
        Ok(models)
    }

    fn wait_for_translation(
        &mut self,
        turn_id: &str,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<String>> {
        let mut final_message = None;
        let model = self.model.clone();
        // A cancellable foreground translation has an explicit user-controlled
        // stop action, so it must not be discarded merely for taking longer.
        let deadline = cancelled
            .is_none()
            .then(|| Instant::now() + TRANSLATION_TIMEOUT);
        loop {
            if is_cancelled(cancelled) {
                return Ok(None);
            }
            let wait = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        bail!("{model} 번역 제한 시간(45초)을 초과했습니다");
                    }
                    remaining
                }
                None => CANCELLATION_POLL_INTERVAL,
            };
            let message = match self.output.recv_timeout(wait) {
                Ok(Ok(message)) => message,
                Ok(Err(message)) => bail!("{model} 번역 응답 대기 실패: {message}"),
                Err(RecvTimeoutError::Timeout) if cancelled.is_some() => continue,
                Err(RecvTimeoutError::Timeout) => {
                    bail!("{model} 번역 제한 시간(45초)을 초과했습니다")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("Codex App Server 응답 스트림이 종료되었습니다")
                }
            };
            self.answer_server_request_if_needed(&message)?;
            match message.get("method").and_then(Value::as_str) {
                Some("item/completed") => {
                    let item = message.pointer("/params/item").unwrap_or(&Value::Null);
                    if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                        final_message = item.get("text").and_then(Value::as_str).map(str::to_owned);
                    }
                }
                Some("turn/completed") => {
                    let turn = message.pointer("/params/turn").unwrap_or(&Value::Null);
                    if turn.get("id").and_then(Value::as_str) != Some(turn_id) {
                        continue;
                    }
                    match turn.get("status").and_then(Value::as_str) {
                        Some("completed") => {
                            return final_message
                                .map(Some)
                                .with_context(|| format!("{model} 번역문을 받지 못했습니다"));
                        }
                        Some("failed") => {
                            let error = turn
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .unwrap_or("알 수 없는 모델 오류");
                            bail!("{model} 번역 실패: {error}");
                        }
                        Some(status) => bail!("{model} 번역이 중단되었습니다: {status}"),
                        None => bail!("{model} 번역 상태를 확인하지 못했습니다"),
                    }
                }
                _ => {}
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_internal(method, params, None)?
            .context("Codex App Server 요청이 취소되었습니다")
    }

    fn request_internal(
        &mut self,
        method: &str,
        params: Value,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<Value>> {
        if is_cancelled(cancelled) {
            return Ok(None);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"method": method, "id": id, "params": params}))?;
        let deadline = cancelled
            .is_none()
            .then(|| Instant::now() + SERVER_RESPONSE_TIMEOUT);

        loop {
            if is_cancelled(cancelled) {
                return Ok(None);
            }
            let wait = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        bail!(
                            "Codex App Server가 {}초 동안 응답하지 않았습니다",
                            SERVER_RESPONSE_TIMEOUT.as_secs()
                        );
                    }
                    remaining
                }
                None => CANCELLATION_POLL_INTERVAL,
            };
            let message = match self.output.recv_timeout(wait) {
                Ok(Ok(message)) => message,
                Ok(Err(message)) => bail!("{message}"),
                Err(RecvTimeoutError::Timeout) if cancelled.is_some() => continue,
                Err(RecvTimeoutError::Timeout) => bail!(
                    "Codex App Server가 {}초 동안 응답하지 않았습니다",
                    SERVER_RESPONSE_TIMEOUT.as_secs()
                ),
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("Codex App Server 응답 스트림이 종료되었습니다")
                }
            };
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && message.get("method").is_none()
            {
                if let Some(error) = message.get("error") {
                    let details = error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| error.to_string());
                    bail!("Codex App Server {method} 실패: {details}");
                }
                return Ok(Some(message.get("result").cloned().unwrap_or(Value::Null)));
            }
            self.answer_server_request_if_needed(&message)?;
        }
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, message)
            .context("Codex App Server 요청 직렬화 실패")?;
        self.input
            .write_all(b"\n")
            .context("Codex App Server 요청 쓰기 실패")?;
        self.input
            .flush()
            .context("Codex App Server 요청 전송 실패")
    }

    fn read_message(&mut self, timeout: Duration) -> Result<Value> {
        match self.output.recv_timeout(timeout) {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(message)) => bail!("{message}"),
            Err(RecvTimeoutError::Timeout) => bail!(
                "Codex App Server가 {}초 동안 응답하지 않았습니다",
                timeout.as_secs()
            ),
            Err(RecvTimeoutError::Disconnected) => {
                bail!("Codex App Server 응답 스트림이 종료되었습니다")
            }
        }
    }

    fn answer_server_request_if_needed(&mut self, message: &Value) -> Result<()> {
        let Some(id) = message.get("id").cloned() else {
            return Ok(());
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(());
        };
        if method.ends_with("requestApproval") {
            self.send(&json!({"id": id, "result": {"decision": "decline"}}))
        } else {
            self.send(&json!({
                "id": id,
                "error": {"code": -32601, "message": "Dog Translate does not expose tools"}
            }))
        }
    }
}

fn resolve_model<'a>(requested: &str, models: &'a [ModelInfo]) -> Option<&'a ModelInfo> {
    models
        .iter()
        .find(|model| model.id == requested)
        .or_else(|| models.iter().find(|model| model.is_default))
        .or_else(|| models.iter().find(|model| model.id == DEFAULT_MODEL))
        .or_else(|| models.first())
}

fn reasoning_efforts(entry: &Value, model_id: &str) -> Vec<String> {
    let mut supported = entry
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|effort| {
            effort.as_str().or_else(|| {
                effort
                    .get("reasoningEffort")
                    .or_else(|| effort.get("effort"))
                    .and_then(Value::as_str)
            })
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if model_id == DEFAULT_MODEL && !supported.iter().any(|effort| effort == "none") {
        // Luna accepts `none` and this preserves the low-latency behavior
        // used by earlier Dog Translate versions.
        supported.push("none".to_owned());
    }
    if let Some(default) = entry
        .get("defaultReasoningEffort")
        .or_else(|| entry.get("reasoningEffort"))
        .and_then(Value::as_str)
        && !supported.iter().any(|effort| effort == default)
    {
        supported.push(default.to_owned());
    }
    if supported.is_empty() {
        supported.push("none".to_owned());
    }

    let mut ordered = Vec::with_capacity(supported.len());
    for preferred in ["none", "minimal", "low", "medium", "high", "xhigh"] {
        if supported.iter().any(|effort| effort == preferred) {
            ordered.push(preferred.to_owned());
        }
    }
    for effort in supported {
        if !ordered.contains(&effort) {
            ordered.push(effort);
        }
    }
    ordered
}

fn resolve_reasoning_effort(requested: Option<&str>, model: &ModelInfo) -> String {
    requested
        .filter(|requested| {
            model
                .reasoning_efforts
                .iter()
                .any(|effort| effort == requested)
        })
        .map(str::to_owned)
        .or_else(|| model.reasoning_efforts.first().cloned())
        .unwrap_or_else(|| "none".to_owned())
}

fn spawn_server_reader(output: ChildStdout) -> Receiver<std::result::Result<Value, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = BufReader::new(output);
        loop {
            let mut line = String::new();
            match output.read_line(&mut line) {
                Ok(0) => {
                    let _ = sender.send(Err(
                        "Codex App Server가 예기치 않게 종료되었습니다".to_owned()
                    ));
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(message) = serde_json::from_str(trimmed)
                        && is_relevant_server_message(&message)
                        && sender.send(Ok(message)).is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(format!("Codex App Server 응답 읽기 실패: {error}")));
                    break;
                }
            }
        }
    });
    receiver
}

fn is_relevant_server_message(message: &Value) -> bool {
    if message.get("id").is_some() {
        return true;
    }
    matches!(
        message.get("method").and_then(Value::as_str),
        Some("account/login/completed" | "item/completed" | "turn/completed")
    )
}

#[derive(Debug, Clone)]
pub struct TranslationBatch {
    pub texts: Vec<String>,
    pub wall_time: Duration,
}

impl Drop for OpenAiClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn spawn_app_server() -> Result<Child> {
    let launcher = find_codex_launcher()?;
    let npm_root = launcher
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let script = npm_root
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("bin")
        .join("codex.js");

    let mut command = if script.is_file() {
        let mut command = Command::new(find_node_executable(npm_root)?);
        command.arg(script).args(["app-server", "--stdio"]);
        command
    } else if launcher.extension().is_none_or(|extension| {
        !extension.eq_ignore_ascii_case("cmd") && !extension.eq_ignore_ascii_case("bat")
    }) {
        let mut command = Command::new(&launcher);
        command.args(["app-server", "--stdio"]);
        command
    } else {
        let invocation = format!("\"\"{}\" app-server --stdio\"", launcher.display());
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C"]).arg(invocation);
        command
    };

    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    hide_console(&mut command);
    #[cfg(debug_assertions)]
    command.stderr(Stdio::inherit());
    #[cfg(not(debug_assertions))]
    command.stderr(Stdio::null());
    if env::var_os("CODEX_HOME").is_none()
        && let Some(user_profile) = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
    {
        command.env("CODEX_HOME", PathBuf::from(user_profile).join(".codex"));
    }
    command
        .spawn()
        .with_context(|| format!("Codex App Server 실행 실패: {}", launcher.display()))
}

fn find_node_executable(npm_root: &std::path::Path) -> Result<PathBuf> {
    for name in if cfg!(windows) {
        &["node.exe", "node"] as &[&str]
    } else {
        &["node", "node.exe"] as &[&str]
    } {
        let bundled = npm_root.join(name);
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    if cfg!(windows)
        && let Some(program_files) = env::var_os("ProgramFiles")
    {
        let installed = PathBuf::from(program_files).join("nodejs").join("node.exe");
        if installed.is_file() {
            return Ok(installed);
        }
    }
    if let Some(path) = find_on_path(if cfg!(windows) { "node.exe" } else { "node" }) {
        return Ok(path);
    }
    bail!("Codex App Server 실행에 필요한 Node.js를 찾지 못했습니다")
}

fn find_codex_launcher() -> Result<PathBuf> {
    if let Some(path) =
        env::var_os("DOG_TRANSLATE_CODEX_PATH").or_else(|| env::var_os("DOF_TRANSLATE_CODEX_PATH"))
    {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    if let Ok(executable) = env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let names: &[&str] = if cfg!(windows) {
            &["codex.exe", "providers/codex.exe", "runtime/codex.exe"]
        } else {
            &["codex", "providers/codex", "runtime/codex"]
        };
        for name in names {
            let path = directory.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }

    if cfg!(windows)
        && let Some(app_data) = env::var_os("APPDATA")
    {
        let path = PathBuf::from(app_data).join("npm").join("codex.cmd");
        if path.is_file() {
            return Ok(path);
        }
    }

    if let Some(profile) = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let profile = PathBuf::from(profile);
        for name in if cfg!(windows) {
            &[".local/bin/codex.exe", ".local/bin/codex"] as &[&str]
        } else {
            &[".local/bin/codex", ".local/bin/codex.exe"] as &[&str]
        } {
            let path = profile.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }

    for name in if cfg!(windows) {
        &["codex.exe", "codex.cmd"] as &[&str]
    } else {
        &["codex"] as &[&str]
    } {
        if let Some(path) = find_on_path(name) {
            return Ok(path);
        }
    }

    Err(anyhow!(
        "Codex CLI를 찾지 못했습니다. ChatGPT/Codex 앱을 설치하거나 DOG_TRANSLATE_CODEX_PATH를 설정하세요"
    ))
}

fn open_browser(url: &str) -> Result<()> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else if cfg!(target_os = "macos") {
        Command::new("open")
    } else {
        Command::new("xdg-open")
    };
    command.arg(OsString::from(url));
    hide_console(&mut command);
    command
        .spawn()
        .context("ChatGPT 로그인 브라우저를 열지 못했습니다")?;
    Ok(())
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

fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = command;
}

fn parse_translation(message: &str) -> Result<String> {
    let trimmed = message.trim();
    if let Ok(parsed) = serde_json::from_str::<StructuredTranslation>(trimmed) {
        return Ok(parsed.translation.trim().to_owned());
    }

    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    if let Ok(parsed) = serde_json::from_str::<StructuredTranslation>(without_fence) {
        return Ok(parsed.translation.trim().to_owned());
    }
    if !trimmed.is_empty() {
        return Ok(trimmed.to_owned());
    }
    bail!("모델 응답이 비어 있습니다")
}

fn parse_translations(message: &str) -> Result<Vec<String>> {
    let trimmed = message.trim();
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    let parsed: StructuredTranslations =
        serde_json::from_str(without_fence).context("일괄 번역 JSON 해석 실패")?;
    Ok(parsed
        .translations
        .into_iter()
        .map(|text| text.trim().to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_translation() {
        let text = parse_translation(r#"{"translation":"서버 점검이 곧 시작됩니다."}"#).unwrap();
        assert_eq!(text, "서버 점검이 곧 시작됩니다.");
    }

    #[test]
    fn parses_fenced_translation() {
        let text = parse_translation("```json\n{\"translation\":\"안녕하세요.\"}\n```").unwrap();
        assert_eq!(text, "안녕하세요.");
    }

    #[test]
    fn parses_batch_translation() {
        let texts = parse_translations(r#"{"translations":["안녕","곧 봐"]}"#).unwrap();
        assert_eq!(texts, ["안녕", "곧 봐"]);
    }

    #[test]
    fn batch_prompt_keeps_one_to_one_lines_and_includes_context() {
        let prompt = TranslationDirection::EnglishToKorean
            .batch_prompt(
                &["ambiguous target".to_owned()],
                &[
                    "previous line".to_owned(),
                    "ambiguous target".to_owned(),
                    "next line".to_owned(),
                ],
            )
            .unwrap();

        assert!(prompt.contains("context_lines"));
        assert!(prompt.contains("previous line"));
        assert!(prompt.contains("줄을 합치거나 한 줄을 여러 항목으로 나누지 마세요"));
    }

    #[test]
    fn filters_unneeded_server_notifications() {
        assert!(is_relevant_server_message(&json!({"id": 7, "result": {}})));
        assert!(is_relevant_server_message(
            &json!({"method": "turn/completed"})
        ));
        assert!(!is_relevant_server_message(
            &json!({"method": "item/agentMessage/delta"})
        ));
    }

    #[test]
    fn resolves_requested_model_then_account_default() {
        let models = vec![
            ModelInfo {
                id: "fast-model".to_owned(),
                display_name: "Fast".to_owned(),
                reasoning_efforts: vec!["none".to_owned(), "low".to_owned()],
                is_default: false,
            },
            ModelInfo {
                id: "account-default".to_owned(),
                display_name: "Default".to_owned(),
                reasoning_efforts: vec!["low".to_owned(), "medium".to_owned()],
                is_default: true,
            },
        ];

        assert_eq!(
            resolve_model("fast-model", &models).unwrap().id,
            "fast-model"
        );
        assert_eq!(
            resolve_model("removed-model", &models).unwrap().id,
            "account-default"
        );
    }

    #[test]
    fn chooses_fastest_supported_reasoning_effort() {
        let entry = json!({
            "supportedReasoningEfforts": [
                {"reasoningEffort": "medium"},
                {"reasoningEffort": "low"},
                {"reasoningEffort": "high"}
            ],
            "defaultReasoningEffort": "medium"
        });
        assert_eq!(
            reasoning_efforts(&entry, "another-model"),
            ["low", "medium", "high"]
        );
    }

    #[test]
    fn keeps_supported_reasoning_choice_and_falls_back_safely() {
        let model = ModelInfo {
            id: "test-model".to_owned(),
            display_name: "Test".to_owned(),
            reasoning_efforts: vec!["low".to_owned(), "medium".to_owned()],
            is_default: false,
        };
        assert_eq!(resolve_reasoning_effort(Some("medium"), &model), "medium");
        assert_eq!(resolve_reasoning_effort(Some("xhigh"), &model), "low");
    }
}
