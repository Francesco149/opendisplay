# OpenDisplay for iOS 12

A receiver build for the devices the App Store app's iOS 15 floor leaves out:
iPad Air (1st gen), iPad mini 2 and 3, iPhone 5s, iPhone 6 and 6 Plus. Same
wire protocol, same `StreamReceiver`, same lifecycle policy and the same
video/touch view as the main app. Only the app shell differs: UIKit instead of
SwiftUI. Use it with the unmodified Mac app.

Tested on an iPad Air (iPad4,1) on iOS 12.5.8 over USB: H.264 at the panel's
native 1536×2048, rotation, touch, roughly 11 ms median capture-to-display
latency.

## What's different from the main app

| Main app (iOS 15+) | Here (iOS 12.2+) |
|---|---|
| SwiftUI `App`, sheets, `@AppStorage` | `LegacyAppDelegate`, `ReceiverViewController`, `SettingsViewController` |
| Combine `ObservableObject` / `@Published` | `CombineShim.swift`: local stand-ins that shadow Combine's names in this module only and post `publishedValueChanged` instead |
| Scenes | App-delegate window on iOS 12, `LegacySceneDelegate` on iOS 13+ (an app built with a current SDK must adopt scenes where they exist) |
| `UILaunchScreen` key | `LaunchScreen.storyboard` (the key is iOS 14+) |
| Version gate, App Store links | Left out: there is no App Store listing for this build |
| HEVC where decodable | H.264 only on these chips. `DecodeBudget` caps A7/A8 at H.264 Level 4.2's macroblock rate, so the Mac streams the full panel at about 42 fps. |

The shared files needed small, behavior-neutral changes to compile for iOS 12:

- `iOS/VideoView.swift` and `iOS/ReceiverModel.swift` were extracted from the
  SwiftUI file. The SwiftUI app wraps `VideoView`; this app hosts it directly.
- `#available` fallbacks for `NWTXTRecord` and `FileHandle.close()`, and a
  `"public.png"` literal instead of `UTType.png`.

## Build

Xcode 27 refuses deployment targets below iOS 15, but its compilers still
emit iOS 12 code, so `build.sh` drives `swiftc`, `actool`, `ibtool` and
`codesign` directly. There is no Xcode target for this app.

```sh
iOSLegacy/build.sh            # device: build/legacy/OpenDisplayLegacy.ipa (+ .app)
iOSLegacy/build.sh --unsigned # device, not signed (what CI builds)
iOSLegacy/build.sh --sim      # simulator: build/legacy-sim/OpenDisplayLegacy.app
iOSLegacy/build.sh --debug    # either, with -Onone and DEBUG
```

The Tests workflow builds `--unsigned`, so a change to the shared receiver
files that needs iOS 13 or Combine fails CI. The simulator build can't catch
that: arm64 simulator slices start at iOS 14.

The device build signs with your "Apple Development" identity and the newest
development profile (a team wildcard profile works) from Xcode's profiles
folder. `DEVELOPMENT_TEAM` comes from `.env`, as for `generate.sh`.

## Install

Xcode 27 doesn't see iOS 12 devices, so it can't install or register them.
Two routes:

- **Development profile.** Add the device's UDID to your team on
  developer.apple.com, refresh the team profile (any Xcode build with
  `-allowProvisioningUpdates`), rebuild with `DEVICE_UDID=<udid>
  iOSLegacy/build.sh`, and install the `.ipa` with any installation_proxy
  client, such as libimobiledevice's `ideviceinstaller -i`. Survives
  reboots, valid for the profile's lifetime.
- **Jailbreak** (checkra1n). Copy the `.app` into `/Applications` and run
  `uicache -p`, skipping installd. It only launches while the device is
  jailbroken. Launch it from SSH with `uiopen opendisplay-legacy://`.

## Testing without a Mac desktop

`tools/fake-sender.swift` drives any receiver with a VideoToolbox test
pattern at the size and rate the receiver's hello allows, and prints the
receiver's own `stats` reports. It needs no Screen Recording permission or
virtual display.

```sh
swiftc -O tools/fake-sender.swift -o build/fake-sender
build/fake-sender --host 127.0.0.1           # simulator
build/fake-sender --usb <udid> --seconds 20  # device, through usbmuxd
```
