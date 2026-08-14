# Dog Translate

Windows와 macOS의 Discord, Element, 브라우저 등에서 화면의 새 영어 문장을 자동으로 감지하고 그 자리에 한국어를 표시하는 앱입니다.

Apache License 2.0으로 배포됩니다. 자세한 내용은 [LICENSE](LICENSE)와 [NOTICE](NOTICE)를 확인하세요.

의존성을 변경한 뒤에는 `cargo about generate --locked -o THIRD_PARTY_LICENSES.html about.hbs`로 제3자 라이선스 보고서를 갱신합니다.

## 현재 동작

1. 번역할 Discord, Element 또는 브라우저 위에 마우스를 둡니다.
2. Windows에서는 `Ctrl+T`, macOS에서는 `⌘T`를 누른 뒤 캡처 화면 상단에서 `한 줄·문단·원문 위치` 단위를 고르고, 계속 번역할 영역을 드래그합니다.
3. 실제 화면으로 돌아간 뒤 Windows OCR 또는 macOS Vision이 영역 안의 영어 줄과 위치를 찾습니다.
4. 모델 응답을 기다리는 동안 각 원문 위치에 `번역 중…`이 먼저 표시됩니다.
5. 번역이 도착하면 같은 위치의 표시가 한국어로 교체되고 영역 변경을 계속 감시합니다.
6. 같은 단축키를 다시 누르면 종료됩니다. 영역 선택 중이나 오버레이 표시 중 `Esc`를 누르면 메인 창을 다시 띄우지 않고 즉시 취소한 뒤 백그라운드에서 대기합니다.

번역 화면은 투명하며 마우스 클릭을 통과시키므로 뒤에 있는 채팅과 웹페이지를 그대로 조작할 수 있습니다. 앱 창 자체는 화면 캡처에서 제외되어 번역문을 다시 OCR하지 않습니다.
기본 `독서형`은 선택한 영역의 가로 폭을 한 줄에서 최대한 사용하고, 오른쪽 끝에 도달한 뒤에만 다음 행으로 자동 줄바꿈합니다. `문단형`은 OCR 줄의 간격·왼쪽 정렬·가로 겹침을 분석해 같은 문단의 번역을 한 블록으로 묶습니다. `원문 덮기형`은 각 OCR 줄의 원래 위치와 폭에 맞춰 표시합니다. 표시 단위는 고급 설정뿐 아니라 영역을 드래그하는 캡처 화면에서도 바로 선택할 수 있습니다. 번역 요청에는 화면의 앞뒤 줄을 함께 전달해 각 줄을 주변 문맥에 맞게 번역합니다.
OCR은 확대된 원본과 `명암 자동 보정·다크 배경 반전·선명화` 이미지를 모두 인식합니다. 두 결과 중 문자 품질이 높은 패스를 기준으로 삼고 다른 패스에서만 발견된 위치의 줄을 보완하되, 같은 위치의 중복 줄은 더 완전한 문자열 하나만 남깁니다. OCR 단어 박스는 큰 간격에서 별도 줄로 다시 나누며, 프레임 간 고정 문장 ID와 좌표 평활화를 적용해 작은 위치 흔들림을 줄입니다. 세로 공간이 부족하면 블록 간격과 글꼴을 순서대로 줄이고, 그래도 부족한 경우 오래된 위쪽 줄부터 숨겨 겹침을 방지합니다.

메인 창은 최근 번역 원문과 결과를 별도로 보관하거나 표시하지 않고 현재 상태와 빠른 작업만 보여 줍니다.

메인 화면의 `번역 계정`에서 `ChatGPT로 로그인` 또는 `Claude로 로그인`을 선택할 수 있습니다. 공급자를 바꾸면 공식 브라우저 로그인이 열리고, 공급자별로 마지막 모델 선택을 따로 기억합니다. 접힌 `고급 설정`에서는 현재 계정에서 사용할 모델, 각 모델이 지원하는 속도·정확도 단계, 화면 배치 방식을 선택할 수 있습니다. 평소에는 `속도 우선`과 `독서형`을 권장합니다. 선택값은 Windows의 `%LOCALAPPDATA%\DogTranslate\settings.json`, macOS의 `~/Library/Application Support/DogTranslate/settings.json`에 저장됩니다. 저장된 모델을 더 이상 사용할 수 없으면 현재 계정에서 유효한 가장 빠른 선택으로 안전하게 대체합니다.

처음 선택하는 모델은 가장 빠른 추론 강도로 시작합니다. 이후에는 메뉴에서 `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` 가운데 해당 모델이 실제로 지원하는 단계만 선택할 수 있습니다. 같은 문장은 위치나 주변 줄이 달라져도 로컬 메모리 캐시에서 즉시 재사용하며, 실제 내용이 새로 생기거나 바뀐 문장만 화면 위에서 아래 순서로 묶어 번역합니다. 캐시는 화면 번역을 끝내거나 앱을 종료하면 사라집니다.

캡처·OCR과 모델 번역은 별도 작업 스레드에서 실행됩니다. 모델 응답을 기다리는 동안에도 OCR은 화면 변경을 계속 감지하고, 대기 중인 번역 요청은 가장 최근 화면 상태로 합쳐집니다. `번역 중…` 블록은 원문 길이로 공간을 미리 예약하며 결과가 도착하면 같은 블록 안의 글자만 교체해 주변 번역문의 움직임을 줄입니다.

일시적인 캡처·OCR·연결 오류는 0.7초부터 최대 10초까지 간격을 늘려 자동 재시도합니다. 화면 번역은 모델 응답이 늦어도 기존 오버레이를 유지하며 계속 기다리고, 55초 뒤에는 중단하는 대신 `응답이 늦지만 계속 기다리는 중` 상태만 표시합니다. 화면 번역 단축키로 끄면 현재 OCR·모델 작업도 함께 취소되어 늦은 결과가 다음 번역 세션에 섞이지 않습니다. 한두 프레임에서 OCR 결과가 비어도 기존 번역을 바로 지우지 않아 화면 깜빡임을 줄입니다. 선택문 번역 역시 완료되거나 사용자가 같은 단축키를 다시 눌러 취소할 때까지 기다립니다.

캡처 이미지는 디스크에 저장하지 않고 메모리에서 바로 OCR 처리합니다.
화면 이미지는 외부로 전송하지 않으며, 운영체제 OCR로 추출한 텍스트만 사용자가 선택한 OpenAI 또는 Anthropic 계정으로 전송합니다.

코드 구조와 Tauri 기반 npm UI 전환안은 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)에 정리되어 있습니다.

## 계정 로그인

API 키는 사용하지 않습니다. 앱 상단에서 다음 중 하나를 선택합니다.

- **ChatGPT**: Codex 사용 권한이 있는 ChatGPT 계정이 필요합니다. 앱은 Codex App Server의 공식 ChatGPT 브라우저 로그인을 사용하며 인증 저장·갱신도 Codex가 담당합니다. 배포 패키지에는 네이티브 Codex 런타임을 넣을 수 있어 Node.js가 필요하지 않습니다.
- **Claude**: Claude Code 사용 권한이 있는 Pro, Max, Team, Enterprise 또는 Console 계정과 공식 Claude Code 설치가 필요합니다. `Claude로 로그인`을 고르면 `claude auth login` 브라우저 흐름을 시작하고, 번역은 비대화형 Claude Code 실행으로 처리합니다. 무료 Claude 계정에는 Claude Code가 포함되지 않습니다.

개발 실행에서는 Codex CLI 또는 `DOG_TRANSLATE_CODEX_PATH`, Claude Code 또는 `DOG_TRANSLATE_CLAUDE_PATH`를 탐색합니다. 이전 `DOF_TRANSLATE_*` 환경 변수도 호환을 위해 계속 인식합니다. Claude 실행 파일은 Anthropic이 배포하는 공식 설치본을 사용하며 이 저장소의 배포 패키지에는 포함하지 않습니다.

## 실행

```powershell
cargo run --release
```

앱 시작 시 마지막으로 선택한 공급자와 계정을 백그라운드에서 확인합니다. 로컬 LLM을 실행하지 않으므로 모델용 RAM/VRAM을 사용하지 않습니다.

## Windows 배포 만들기

Windows용 ZIP과 단일 휴대용 실행 파일은 다음 명령으로 만듭니다.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1
```

결과는 `dist` 폴더에 생성됩니다.

- `DogTranslate-<버전>-Windows-x64.zip`: 앱과 `providers/codex.exe`가 들어 있는 일반 배포본
- `DogTranslate-<버전>-Windows-x64-Portable.exe`: Windows IExpress로 묶은 단일 실행 파일

단일 파일은 실행할 때 임시 폴더로 압축을 푼 뒤 앱을 시작하므로 ZIP 배포본보다 시작이 느립니다. 실제 공개 배포 전에는 Windows 코드 서명을 권장합니다. Claude를 사용하려는 사용자는 공식 Claude Code를 별도로 설치한 뒤 앱에서 Claude를 선택하면 됩니다.

OCR·번역 파이프라인만 진단하려면 영어가 들어 있는 이미지 경로를 넘길 수 있습니다.

```powershell
cargo run -- --translate-image "C:\path\to\english.png"
cargo run -- --translate-korean "회의가 10분 늦어질 것 같아요."
```

영어 OCR 언어 팩이 없으면 Windows 사용자 프로필의 OCR 언어 엔진으로 자동 대체합니다.

## Apple Silicon macOS 배포 만들기

Apple Silicon(M1 이상) Mac에서 다음 명령을 실행하면 ARM64 전용 `.app`, ZIP, DMG가 생성됩니다. ChatGPT를 바로 사용할 수 있는 독립 배포본을 만들려면 먼저 네이티브 Codex를 설치합니다.

```bash
npm install -g @openai/codex
chmod +x ./scripts/package-macos.sh
./scripts/package-macos.sh
```

결과는 `dist` 폴더에 생성됩니다.

- `DogTranslate.app`
- `DogTranslate-<버전>-macOS-AppleSilicon.zip`
- `DogTranslate-<버전>-macOS-AppleSilicon.dmg`

Windows에서는 Apple SDK, `codesign`, `hdiutil`을 사용할 수 없으므로 실행 가능한 macOS 앱과 DMG를 완성할 수 없습니다. `.github/workflows/build-macos-apple-silicon.yml`을 GitHub 저장소에서 수동 실행하면 Apple Silicon macOS 러너가 동일한 배포 파일을 빌드해 작업 결과물로 제공합니다.

화면 번역에는 macOS의 **화면 기록**, 선택문 자동 복사에는 **손쉬운 사용** 권한이 필요합니다. 스크립트는 기본적으로 ad-hoc 서명하며, 다른 사람에게 배포할 때는 `DOG_TRANSLATE_CODESIGN_IDENTITY`에 Developer ID Application 인증서 이름을 지정한 뒤 Apple notarization을 진행해야 Gatekeeper 경고를 줄일 수 있습니다. Claude는 공식 Claude Code 설치본을 탐색하고, ChatGPT는 네이티브 Codex를 찾으면 앱 번들에 포함합니다.

실시간 번역은 Windows의 `Ctrl+T`, macOS의 `⌘T`로 켜고 끕니다. 앱이 실행 중이면 이 전역 단축키가 현재 앱의 일반 단축키보다 우선합니다.

## 빠른 입력 번역

### 선택문을 번역해 바로 붙여넣기

Discord, Element, 브라우저 등의 입력창에서 `Ctrl+A` 또는 마우스로 한국어 문장을 선택한 뒤 `Ctrl+B`를 누릅니다.

1. 화면 상단에 `번역 중…` 알림이 표시됩니다.
2. 번역이 끝나면 클립보드에 영어 번역이 저장되고 `붙여넣기 준비 완료`로 바뀝니다.
3. 원래 입력창에서 `Ctrl+V`를 누르면 번역문이 바로 붙여넣어집니다.

번역 중 `Ctrl+B`를 한 번 더 누르면 즉시 취소됩니다. 진행 중이던 모델 연결도 종료하고, 늦게 도착한 결과는 클립보드에 쓰지 않습니다. 취소 직후 다른 문장을 선택해 새 번역을 시작할 수 있습니다.

상단 알림은 포커스를 가져가지 않으며 마우스 클릭도 통과합니다.

## 단축키

| 기능 | 단축키 |
|---|---|
| 선택 영역 실시간 영어 → 한국어 | `Ctrl+T` |
| 선택한 한국어 → 영어 번역 준비 / 진행 중 취소 | `Ctrl+B` |
| 영역 선택·화면 오버레이 취소 후 백그라운드 대기 | `Esc` |

macOS에서는 위 표의 `Ctrl`을 `⌘`로 사용합니다.
