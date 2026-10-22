#!/bin/zsh
# Build the iOS 12 legacy receiver (OpenDisplay for iPad Air, iPad mini 2/3,
# iPhone 5s/6) without Xcode's build system, which refuses deployment
# targets below iOS 15 since Xcode 27. The toolchain underneath still emits
# iOS 12 code, so this drives swiftc, actool, ibtool and codesign directly.
#
#   iOSLegacy/build.sh            device build, signed, -> build/legacy/OpenDisplayLegacy.ipa
#   iOSLegacy/build.sh --unsigned device build, not signed (CI), -> build/legacy/OpenDisplayLegacy.app
#   iOSLegacy/build.sh --sim      simulator build, unsigned, -> build/legacy-sim/OpenDisplayLegacy.app
#   iOSLegacy/build.sh --debug    -Onone + DEBUG (either platform)
#
# Only the device build compiles against the iOS 12.2 floor; the simulator
# has no arm64 slice below iOS 14, so --sim can't catch an iOS 13 API.
#
# Signing uses an "Apple Development" identity and a development provisioning
# profile from Xcode's profile folder whose app ID covers BUNDLE_ID (a team
# wildcard profile does). DEVELOPMENT_TEAM comes from .env, as for
# generate.sh. DEVICE_UDID, if set, must be in the profile. Environment:
#   DEVELOPMENT_TEAM  team ID (else the first matching profile's team)
#   BUNDLE_ID         default com.peetzweg.opensidecar.ios.legacy
#   DEVICE_UDID       refuse profiles that don't list this device
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -f .env ]] && export $(grep -v '^#' .env | xargs)

SIM=0
SIGN=1
CONFIG=release
for arg in "$@"; do
  case $arg in
    --sim) SIM=1 ;;
    --unsigned) SIGN=0 ;;
    --debug) CONFIG=debug ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

NAME=OpenDisplayLegacy
BUNDLE_ID=${BUNDLE_ID:-com.peetzweg.opensidecar.ios.legacy}
# 12.2 is the first iOS with the Swift runtime in the OS: nothing to embed.
MIN_OS=12.2
VERSION=$(git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//' || true)
VERSION=${VERSION:-0.0.0}
BUILD_NUMBER=$(git rev-list --count HEAD 2>/dev/null || echo 1)

if (( SIM )); then
  SDK_NAME=iphonesimulator
  PLATFORM=iPhoneSimulator
  # arm64 simulator slices start at iOS 14.
  TRIPLE=arm64-apple-ios14.0-simulator
  OUT=build/legacy-sim
else
  SDK_NAME=iphoneos
  PLATFORM=iPhoneOS
  TRIPLE=arm64-apple-ios$MIN_OS
  OUT=build/legacy
fi
SDK=$(xcrun --sdk $SDK_NAME --show-sdk-path)
SDK_VERSION=$(xcrun --sdk $SDK_NAME --show-sdk-version)
APP=$OUT/$NAME.app
WORK=$OUT/intermediates
rm -rf "$APP" "$WORK"
mkdir -p "$APP" "$WORK"

# The legacy target's sources: its own UIKit screens plus the main app's
# Combine-free files (see the comments at the top of each).
SOURCES=(
  iOSLegacy/*.swift
  iOS/DecodeBudget.swift
  iOS/Log.swift
  iOS/MetalVideoRenderer.swift
  iOS/ReceiverModel.swift
  iOS/VideoView.swift
  Shared/AppStore.swift
  Shared/DirectCable.swift
  Shared/IdleFrameDumper.swift
  Shared/LogSnapshot.swift
  Shared/Protocol.swift
  Shared/StreamReceiver.swift
)

SWIFT_FLAGS=(-sdk "$SDK" -target $TRIPLE -module-name $NAME -swift-version 5
             -D OPENDISPLAY_LEGACY)
if [[ $CONFIG == debug ]]; then
  SWIFT_FLAGS+=(-Onone -g -D DEBUG)
else
  SWIFT_FLAGS+=(-O -wmo)
fi

echo "==> swiftc ($TRIPLE, $CONFIG)"
xcrun --sdk $SDK_NAME swiftc "${SWIFT_FLAGS[@]}" "${SOURCES[@]}" -o "$APP/$NAME"

echo "==> actool / ibtool"
xcrun actool iOS/Assets.xcassets --compile "$APP" --platform $SDK_NAME \
  --minimum-deployment-target $MIN_OS --app-icon AppIcon \
  --target-device iphone --target-device ipad \
  --output-partial-info-plist "$WORK/assets.plist" \
  --output-format human-readable-text >/dev/null
xcrun ibtool --compile "$APP/LaunchScreen.storyboardc" iOSLegacy/LaunchScreen.storyboard \
  --minimum-deployment-target $MIN_OS --target-device iphone --target-device ipad \
  --output-format human-readable-text >/dev/null

echo "==> Info.plist"
python3 -I - iOSLegacy/Info.plist "$WORK/assets.plist" "$APP/Info.plist" <<EOF
import plistlib, sys
template, assets, out = sys.argv[1:]
info = plistlib.load(open(template, 'rb'))
info.update(plistlib.load(open(assets, 'rb')))
info.update({
    'CFBundleExecutable': '$NAME',
    'CFBundleIdentifier': '$BUNDLE_ID',
    'CFBundleName': '$NAME',
    'CFBundleShortVersionString': '$VERSION',
    'CFBundleVersion': '$BUILD_NUMBER',
    'CFBundleSupportedPlatforms': ['$PLATFORM'],
    'MinimumOSVersion': '$MIN_OS',
    'DTPlatformName': '$SDK_NAME',
    'DTPlatformVersion': '$SDK_VERSION',
    'DTSDKName': '$SDK_NAME$SDK_VERSION',
})
plistlib.dump(info, open(out, 'wb'), fmt=plistlib.FMT_BINARY)
EOF
printf 'APPL????' > "$APP/PkgInfo"

if (( SIM )); then
  codesign --force --sign - --timestamp=none "$APP"
  echo "built $APP"
  exit 0
fi
if (( ! SIGN )); then
  echo "built $APP (unsigned)"
  exit 0
fi

echo "==> signing"
# Pick the newest valid development profile covering BUNDLE_ID (and
# DEVICE_UDID if set), and the installed identity whose certificate it lists.
PROFILES="$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"
SIGNING=$(python3 -I - "$PROFILES" "$BUNDLE_ID" "${DEVELOPMENT_TEAM:-}" "${DEVICE_UDID:-}" <<'EOF'
import datetime, fnmatch, hashlib, os, plistlib, subprocess, sys
folder, bundle_id, team, udid = sys.argv[1:]
identities = subprocess.run(['security', 'find-identity', '-v', '-p', 'codesigning'],
                            capture_output=True, text=True).stdout
installed = {line.split()[1] for line in identities.splitlines() if '"' in line}
now = datetime.datetime.now(datetime.timezone.utc)
best = None
for name in os.listdir(folder) if os.path.isdir(folder) else []:
    if not name.endswith('.mobileprovision'):
        continue
    path = os.path.join(folder, name)
    der = subprocess.run(['security', 'cms', '-D', '-i', path], capture_output=True).stdout
    try:
        p = plistlib.loads(der)
    except Exception:
        continue
    ent = p.get('Entitlements', {})
    prefix = p.get('TeamIdentifier', [''])[0]
    expires = p['ExpirationDate'].replace(tzinfo=datetime.timezone.utc)
    if (team and prefix != team) or expires < now or not ent.get('get-task-allow'):
        continue
    if not fnmatch.fnmatchcase(f'{prefix}.{bundle_id}', ent.get('application-identifier', '')):
        continue
    if udid and udid not in p.get('ProvisionedDevices', []):
        continue
    certs = [hashlib.sha1(c).hexdigest().upper() for c in p.get('DeveloperCertificates', [])]
    signer = next((c for c in certs if c in installed), None)
    if signer and (best is None or expires > best[0]):
        best = (expires, path, signer, prefix)
if best is None:
    sys.exit(f'no usable development profile for {bundle_id}'
             + (f' listing {udid}' if udid else '') + f' in {folder}')
print(best[1], best[2], best[3], sep='\t')
EOF
)
IFS=$'\t' read -r PROFILE IDENTITY TEAM <<<"$SIGNING"
echo "profile: $PROFILE"
echo "identity: $IDENTITY (team $TEAM)"
cp "$PROFILE" "$APP/embedded.mobileprovision"
cat > "$WORK/entitlements.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>application-identifier</key>
	<string>$TEAM.$BUNDLE_ID</string>
	<key>com.apple.developer.team-identifier</key>
	<string>$TEAM</string>
	<key>get-task-allow</key>
	<true/>
	<key>keychain-access-groups</key>
	<array>
		<string>$TEAM.$BUNDLE_ID</string>
	</array>
</dict>
</plist>
EOF
codesign --force --sign "$IDENTITY" --entitlements "$WORK/entitlements.plist" \
  --generate-entitlement-der --timestamp=none "$APP"
codesign --verify --strict "$APP"

echo "==> ipa"
rm -rf "$WORK/Payload" "$OUT/$NAME.ipa"
mkdir -p "$WORK/Payload"
cp -R "$APP" "$WORK/Payload/"
(cd "$WORK" && zip -qry "../$NAME.ipa" Payload)
echo "built $OUT/$NAME.ipa"
