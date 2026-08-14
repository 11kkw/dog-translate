#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Apple Silicon 패키지는 macOS에서 빌드해야 합니다." >&2
  exit 1
fi

host_arch="$(uname -m)"
if [[ "$host_arch" != "arm64" ]]; then
  echo "현재 Mac은 $host_arch 아키텍처입니다. Apple Silicon(arm64) Mac에서 실행하세요." >&2
  exit 1
fi

for command_name in cargo codesign ditto file hdiutil rustup xcrun; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "필수 도구를 찾지 못했습니다: $command_name" >&2
    exit 1
  fi
done

project_root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -nE 's/^version[[:space:]]*=[[:space:]]*"([^"]+)"/\1/p' "$project_root/Cargo.toml" | head -n 1)"
target="aarch64-apple-darwin"
bundle_name="DogTranslate-${version}-macOS-AppleSilicon"
dist_root="$project_root/dist"
app_root="$dist_root/DogTranslate.app"
contents="$app_root/Contents"
macos_root="$contents/MacOS"
resources_root="$contents/Resources"
providers_root="$macos_root/providers"
release_binary="$project_root/target/$target/release/dog-translate"

if [[ -z "$version" ]]; then
  echo "Cargo.toml에서 앱 버전을 찾지 못했습니다." >&2
  exit 1
fi

rustup target add "$target"
cargo build --manifest-path "$project_root/Cargo.toml" --release --target "$target"

mkdir -p "$dist_root"
case "$app_root" in
  "$dist_root"/*) ;;
  *) echo "안전하지 않은 앱 출력 경로입니다: $app_root" >&2; exit 1 ;;
esac
if [[ -e "$app_root" ]]; then
  rm -rf -- "$app_root"
fi
mkdir -p "$macos_root" "$resources_root" "$providers_root"

cp "$release_binary" "$macos_root/DogTranslate"
chmod 755 "$macos_root/DogTranslate"
cp "$project_root/packaging/README.txt" "$resources_root/README.txt"
cp "$project_root/LICENSE" "$resources_root/LICENSE.txt"
cp "$project_root/NOTICE" "$resources_root/NOTICE"
cp "$project_root/THIRD_PARTY_LICENSES.html" "$resources_root/THIRD_PARTY_LICENSES.html"

cat > "$contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>ko</string>
  <key>CFBundleDisplayName</key><string>Dog Translate</string>
  <key>CFBundleExecutable</key><string>DogTranslate</string>
  <key>CFBundleIdentifier</key><string>com.dogtranslate.app</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Dog Translate</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSArchitecturePriority</key><array><string>arm64</string></array>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSScreenCaptureUsageDescription</key><string>선택한 화면 영역을 OCR로 번역하기 위해 화면 기록 권한이 필요합니다.</string>
</dict>
</plist>
EOF

codex_binary=""
if [[ -n "${DOG_TRANSLATE_CODEX_PATH:-}" && -f "$DOG_TRANSLATE_CODEX_PATH" ]]; then
  codex_binary="$DOG_TRANSLATE_CODEX_PATH"
elif [[ -n "${DOF_TRANSLATE_CODEX_PATH:-}" && -f "$DOF_TRANSLATE_CODEX_PATH" ]]; then
  codex_binary="$DOF_TRANSLATE_CODEX_PATH"
fi
if [[ -z "$codex_binary" ]] && command -v npm >/dev/null 2>&1; then
  npm_root="$(npm root -g 2>/dev/null || true)"
  if [[ -d "$npm_root/@openai/codex/node_modules" ]]; then
    codex_binary="$(find "$npm_root/@openai/codex/node_modules" -type f -name codex -path '*/vendor/*/codex/codex' -print -quit 2>/dev/null || true)"
  fi
fi
if [[ -z "$codex_binary" ]] && command -v codex >/dev/null 2>&1; then
  codex_command="$(command -v codex)"
  if file "$codex_command" | grep -q 'Mach-O'; then
    codex_binary="$codex_command"
  fi
fi

if [[ -n "$codex_binary" ]]; then
  codex_arch="$(file "$codex_binary")"
  if [[ "$codex_arch" != *"arm64"* ]]; then
    echo "찾은 Codex 실행 파일이 Apple Silicon용이 아닙니다: $codex_arch" >&2
    exit 1
  fi
  cp "$codex_binary" "$providers_root/codex"
  chmod 755 "$providers_root/codex"
elif [[ "${DOG_TRANSLATE_ALLOW_MISSING_CODEX:-0}" != "1" ]]; then
  echo "Apple Silicon용 네이티브 Codex를 찾지 못했습니다." >&2
  echo "npm install -g @openai/codex 또는 DOG_TRANSLATE_CODEX_PATH 설정 후 다시 실행하세요." >&2
  exit 1
else
  echo "경고: Codex 없이 패키징합니다. ChatGPT 번역은 Codex를 별도로 설치해야 동작합니다." >&2
fi

sign_identity="${DOG_TRANSLATE_CODESIGN_IDENTITY:-${DOF_TRANSLATE_CODESIGN_IDENTITY:--}}"
sign_component() {
  local component="$1"
  if [[ "$sign_identity" == "-" ]]; then
    codesign --force --sign - "$component"
  else
    codesign --force --options runtime --timestamp --sign "$sign_identity" "$component"
  fi
}

if [[ -f "$providers_root/codex" ]]; then
  sign_component "$providers_root/codex"
fi
sign_component "$app_root"
codesign --verify --deep --strict --verbose=2 "$app_root"

notary_profile="${DOG_TRANSLATE_NOTARY_PROFILE:-}"
if [[ -n "$notary_profile" ]]; then
  if [[ "$sign_identity" == "-" ]]; then
    echo "공증에는 Developer ID Application 서명이 필요합니다." >&2
    exit 1
  fi
  notary_upload="$dist_root/${bundle_name}-notarization.zip"
  rm -f -- "$notary_upload"
  ditto -c -k --sequesterRsrc --keepParent "$app_root" "$notary_upload"
  xcrun notarytool submit "$notary_upload" --keychain-profile "$notary_profile" --wait
  xcrun stapler staple "$app_root"
  xcrun stapler validate "$app_root"
  rm -f -- "$notary_upload"
fi

zip_path="$dist_root/${bundle_name}.zip"
dmg_path="$dist_root/${bundle_name}.dmg"
rm -f -- "$zip_path" "$dmg_path"
ditto -c -k --sequesterRsrc --keepParent "$app_root" "$zip_path"

dmg_staging="$(mktemp -d "${TMPDIR:-/tmp}/dog-translate-dmg.XXXXXX")"
cleanup() {
  rm -rf -- "$dmg_staging"
}
trap cleanup EXIT
cp -R "$app_root" "$dmg_staging/Dog Translate.app"
ln -s /Applications "$dmg_staging/Applications"
hdiutil create -volname "Dog Translate" -srcfolder "$dmg_staging" -ov -format UDZO "$dmg_path"

if [[ "$sign_identity" != "-" ]]; then
  codesign --force --timestamp --sign "$sign_identity" "$dmg_path"
fi

if [[ -n "$notary_profile" ]]; then
  xcrun notarytool submit "$dmg_path" --keychain-profile "$notary_profile" --wait
  xcrun stapler staple "$dmg_path"
  xcrun stapler validate "$dmg_path"
fi

echo "Apple Silicon 앱: $app_root"
echo "Apple Silicon ZIP: $zip_path"
echo "Apple Silicon DMG: $dmg_path"
if [[ "$sign_identity" == "-" ]]; then
  echo "현재 ad-hoc 서명입니다. 공개 배포 전 Developer ID 서명과 notarization을 적용하세요."
elif [[ -z "$notary_profile" ]]; then
  echo "Developer ID 서명은 적용됐지만 공증은 생략됐습니다. DOG_TRANSLATE_NOTARY_PROFILE을 설정하세요."
fi
