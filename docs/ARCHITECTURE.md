# Dog Translate 구조와 전환 계획

## 현재 구조

| 파일 | 책임 |
|---|---|
| `src/main.rs` | 실행 진입점만 담당 |
| `src/app.rs` | 앱 상태, 작업 조정, 네이티브 창 생명주기 |
| `src/capture.rs` | 모니터 캡처 |
| `src/ocr.rs` | Windows OCR, 단어 박스 기반 줄 재분할 |
| `src/overlay.rs` | 프레임 간 줄 추적, 좌표 평활화, 라벨 자동 배치 |
| `src/openai.rs` | Codex App Server 연결과 번역 |
| `src/clipboard.rs` | 선택문 복사·붙여넣기 |
| `src/settings.rs` | 설정 읽기·검증·저장 |
| `src/ui_components.rs` | 테마와 재사용 UI 구성요소 |

## OCR/오버레이 전략

기본 엔진은 메모리 사용량이 낮고 설치가 필요 없는 Windows OCR을 유지한다.

1. 확대 원본과 자동 명암 보정·다크 배경 반전·선명화 이미지를 Windows OCR 한 엔진에서 이중 인식한다.
2. 문자 완성도가 높은 패스를 기준으로 선택하고 공간적으로 누락된 줄만 보조 패스에서 병합하며, 동일 위치 중복은 제거한다.
3. OCR 단어 박스를 큰 수평 간격에서 다시 나눠 서로 다른 열이나 UI 요소가 하나의 긴 줄로 합쳐지는 문제를 줄인다.
4. 이전 프레임과 현재 프레임을 문자열 유사도, IoU, 중심 거리로 연결하고 좌표 평활화를 적용한다.
5. OCR 원문 한 줄과 번역 블록 하나의 1:1 대응을 유지하고, 전체 화면 줄을 문맥으로 함께 보내되 출력 순서와 개수를 고정한다.
6. 번역문 폭과 실제 그리기 클립을 사용자가 선택한 원문 열 안으로 제한하고, 긴 번역만 블록 내부에서 세로로 줄바꿈한다.
7. 정규화한 원문 문자열을 캐시 키로 사용하고 프레임 간 문장 ID를 유지해, 위치나 주변 문맥만 바뀐 줄은 재번역하지 않는다.
8. OCR 전용 작업자와 모델 번역 작업자를 분리한다. 모델 응답 중에도 다음 캡처를 OCR하고, 번역 작업자는 최신 미번역 문장 묶음만 이어서 처리한다.
9. 기본 `독서형`은 선택 영역 전체 폭과 고정 왼쪽 기준선을 사용한다. `문단형`은 줄 간격·왼쪽 정렬·가로 겹침으로 문단 경계를 추정하며, `원문 덮기형`은 OCR 줄의 X 좌표와 폭을 사용한다. 세 모드는 영역 선택 화면과 고급 설정에서 모두 바꿀 수 있다.
10. 번역 대기 중에는 원문 텍스트 크기로 블록 공간을 예약한다. 세로 공간이 부족하면 간격 축소, 글꼴 축소, 오래된 위쪽 줄 숨김 순서로 대응한다.

Windows OCR이 글자 자체를 자주 놓치는 환경에는 `PP-OCRv5_mobile_det` 같은 경량 검출기를 선택형 엔진으로 추가한다. DBNet/CRAFT 계열은 기울거나 불규칙한 글자 영역 검출에 유리하지만 모델 파일과 추론 런타임 비용이 있으므로 기본 경로에는 넣지 않는다.

참고:

- Windows OCR 단어 좌표: <https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrword>
- PaddleOCR 텍스트 검출: <https://www.paddleocr.ai/main/en/version3.x/module_usage/text_detection.html>
- DBNet: <https://arxiv.org/abs/1911.08947>
- CRAFT: <https://arxiv.org/abs/1904.01941>
- ByteTrack 데이터 연관: <https://arxiv.org/abs/2110.06864>

## npm 기반 데스크톱 UI 권장안

Electron 전체 전환은 Chromium과 Node.js 런타임을 함께 배포하므로 이 앱의 가벼움 목표와 맞지 않는다. npm 프런트엔드가 필요하면 Tauri 2를 사용한다.

권장하는 하이브리드 구조:

```text
Svelte/TypeScript 설정창 (Tauri WebView)
                │ IPC
                ▼
Rust dof-core
  ├─ 캡처 / OCR / 번역 / 단축키
  └─ 네이티브 투명 오버레이
```

설정창과 온보딩만 Svelte/TypeScript로 만들고, 실시간 투명 오버레이는 Rust 네이티브 창으로 유지한다. 이렇게 하면 UI 개발성은 높이면서 화면 캡처, 클릭 통과, 최상단 창, 빠른 그리기 경로는 보존할 수 있다.

전환 순서:

1. `app.rs`에서 작업자와 상태 머신을 `engine` 라이브러리로 추가 분리한다.
2. Rust 명령 API와 이벤트 타입을 정의한다.
3. `apps/desktop`에 Tauri 2 + Svelte 프런트엔드를 추가한다.
4. 설정창을 먼저 교체하고 네이티브 오버레이는 그대로 연결한다.
5. Windows와 macOS의 캡처·OCR 구현을 플랫폼 어댑터로 분리한다.

Tauri는 OS WebView와 Rust 백엔드를 사용하며 브라우저 엔진을 앱마다 포함하지 않는다. Electron은 Chromium 기반 다중 프로세스 구조다.

- Tauri 구조: <https://tauri.app/start/>
- Electron 프로세스 모델: <https://www.electronjs.org/docs/latest/tutorial/process-model>
