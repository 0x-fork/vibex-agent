#!/usr/bin/env bash
#
# Packages the unsigned iOS device IPA.
#
# The point of this artifact is that producing it needs no Apple account, no
# certificate, and no provisioning profile: it is how someone with no developer
# account still ships an iOS build. iOS refuses to run unsigned code, so the
# file cannot be installed as-is — whoever installs it re-signs it with their
# own Apple ID. See docs/operations/ios-unsigned-install.md.
#
# The build itself still needs macOS and Xcode, because the device link step
# resolves the iOS SDK through xcrun.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT"

XCFRAMEWORK="apps/mobile/ios/VibexFFI.xcframework"
PROJECT="apps/mobile/ios/Vibex.xcodeproj"
DERIVED_DATA="target/mobile-ios"
OUTPUT_DIR="${VIBEX_MOBILE_OUTPUT_DIR:-target/mobile-ios-artifacts}"

command -v xcodebuild >/dev/null 2>&1 || {
  echo "xcodebuild is required (this step runs on macOS with Xcode installed)" >&2
  exit 1
}

# The release workflow passes the version it stamped into the tag; a local run
# falls back to the version the mobile crate already declares.
resolve_version() {
  if [[ -n "${VIBEX_MOBILE_VERSION:-}" ]]; then
    printf '%s\n' "${VIBEX_MOBILE_VERSION}"
    return
  fi
  # The package version is the first `version =` line in the manifest. Fall back
  # to the workspace table so an inherited `version.workspace = true` keeps
  # working instead of silently resolving to an empty string.
  local version
  version="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' apps/mobile/Cargo.toml)"
  if [[ -z "$version" ]]; then
    version="$(awk -F'"' '/^\[workspace\.package\]/{found=1; next} /^\[/{found=0} found && /^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
  fi
  printf '%s\n' "$version"
}

VERSION="$(resolve_version)"
if [[ -z "$VERSION" ]]; then
  echo "could not resolve a marketing version; set VIBEX_MOBILE_VERSION" >&2
  exit 1
fi

# A local run should be one command, so generate the native inputs when they are
# not there yet. CI builds them in its own step and skips this.
if [[ ! -d "$XCFRAMEWORK" || ! -d "$PROJECT" ]]; then
  echo "generating ${XCFRAMEWORK} and ${PROJECT}" >&2
  VIBEX_MOBILE_PROFILE=release bash apps/mobile/scripts/build-ios.sh
fi

# Signing stays disabled for the same reason the simulator build disables it:
# the artifact is unsigned on purpose, and a build that quietly acquired a
# signature would no longer be reproducible without credentials.
xcodebuild -project "$PROJECT" \
  -scheme Vibex \
  -sdk iphoneos \
  -configuration Release \
  MARKETING_VERSION="$VERSION" \
  -derivedDataPath "$DERIVED_DATA" \
  CODE_SIGNING_ALLOWED=NO \
  CODE_SIGNING_REQUIRED=NO \
  build

APP="${DERIVED_DATA}/Build/Products/Release-iphoneos/Vibex.app"
if [[ ! -d "$APP" ]]; then
  echo "expected the device bundle at ${APP}" >&2
  exit 1
fi

# Refuse to put the "unsigned" name on a bundle that actually carries signing
# material: that would hand someone a file that silently behaves differently
# from what the release documents.
if [[ -e "${APP}/_CodeSignature" || -e "${APP}/embedded.mobileprovision" ]]; then
  echo "refusing to publish a signed bundle as an unsigned IPA" >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
IPA="${OUTPUT_DIR}/vibex-${VERSION}-ios-unsigned.ipa"

STAGING="$(mktemp -d "${TMPDIR:-/tmp}/vibex-ios-ipa.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
mkdir -p "${STAGING}/Payload"
ditto "$APP" "${STAGING}/Payload/Vibex.app"

# An IPA is a zip whose root holds a Payload directory. `--keepParent` is what
# embeds that directory name; without it the archive would flatten to the app
# bundle and no signing tool would accept it.
(cd "$STAGING" && ditto -c -k --sequesterRsrc --keepParent Payload "$IPA")

# Match inside the shell instead of piping through `unzip -l | grep -q`. Under
# `pipefail` that pipeline depends on how the platform's unzip reacts to a
# reader that exits at the first match, which is not worth betting a release on.
listing="$(unzip -l "$IPA")"
if [[ "$listing" != *"Payload/Vibex.app/Info.plist"* ]]; then
  echo "packaged IPA has no Payload/Vibex.app root" >&2
  exit 1
fi

(cd "$OUTPUT_DIR" && shasum -a 256 "$(basename "$IPA")" > "$(basename "$IPA").sha256")

printf '%s\n' "$IPA"
echo "unsigned: the installer re-signs it with their own Apple ID" >&2
echo "see docs/operations/ios-unsigned-install.md" >&2
