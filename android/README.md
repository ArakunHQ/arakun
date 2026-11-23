# Arakun for Android

A native Jetpack Compose app for your sessions on the go: you sign in with GitHub, pick
one of your account's computers, and it speaks the phone page's (`../phone/`) protocol
through the relay, end to end encrypted. It does what the iOS app does, screen for
screen.

```
android/
  kit/               the protocol, plain Kotlin, testable on any JVM
    Account.kt       GitHub's device flow, /me, /machines, /sessions (core/src/cloud.rs)
    E2E.kt           P-256 handshake, HKDF keys, AES-GCM frames (phone/e2e.js)
    RelayLink.kt     the WebSocket to the relay, pings, {req}/{res}, {part} (phone/relay.js)
    Pairing.kt       why a channel ended, and what an opened link gets told
    Models.kt        inbox, session, entries, state (core/src/phone/wire.rs)
  app/               the app: sign-in, computers, inbox, a live session, new session
  tools/vectors.py   the brand's SVG icons and the raccoon as vector drawables
```

## Building

JDK 17 or newer and the Android SDK with platform 37.2 (`sdkmanager "platforms;android-37.2"`),
or Android Studio. Point Gradle at the SDK with `ANDROID_HOME` or `local.properties`
(`sdk.dir=...`).

```sh
cd android
./gradlew :kit:test            # the protocol against phone/test/vectors, a played relay and account host
./gradlew :app:assembleDebug   # app/build/outputs/apk/debug/app-debug.apk
./gradlew :app:lintDebug
```

Without an account or a computer, start a debug build with `demo`: it is signed in to a
sample account with two computers, and sample sessions fill the inbox. `demoOpen s1`
opens a session, `demoNew true` the New session sheet, `demoMachines true` the list of
computers, `demoUnpaired true` the sign-in screen and `demoMoods true` every mood of the
raccoon side by side:

```sh
adb shell am start -n com.arakun.app/com.arakun.android.MainActivity --ez demo true --es demoOpen s1
adb shell cmd uimode night yes   # or no: the app follows the system
```

## Design

The app follows the brand book (`.claude/skills/arakun-brand`): black and white grounds
with no tint, amber only for the one primary button and the live dot, Geist and Geist
Mono, the line icons from `assets/icons`, and the raccoon. Every color, font and corner
comes from `app/src/main/java/com/arakun/android/ui/Theme.kt`; it follows the system's
dark or light setting. The launcher icon is the raccoon on ink, with a one-tone version
for themed icons.

The fonts and `assets/brand/mascot.json` are copied in at build time
(`app/build.gradle.kts`), not kept here, so the app and the desktop never drift apart.
The icons are vectors made from `assets/icons` by `tools/vectors.py`; run it again
after an icon changes or the app uses a new one (list it in `USED` there).

The raccoon is alive, as on the desktop and on iOS: `ui/Mascot.kt` is a smaller port of
`core/src/mascot.rs`. It blinks and looks around, and shows the mood of what it
watches: thinking dots, reading while the agent works, a hopping question mark when a
session needs you, a tick when a turn ends, a shake and an exclamation mark on an
error, and sleep when the computer is offline or nothing happens for two minutes. A tap
makes it hop. One raccoon per screen: the sign-in screen, the inbox header (the mood of
all sessions) and the composer of a session (that session's mood). With "Remove
animations" on, it holds still.

## CI

CI's Android job (`.github/workflows/ci.yml`) runs the kit's and the app's tests, lint,
a debug build and a release bundle (`.aab`) signed with a throwaway key, when
`android/`, `phone/`, the mascot's drawing or the fonts change.

`.github/workflows/android-release.yml` delivers the app: after a release is published
(release.yml calls it on the `v*` tag, not for a pre-release), or by hand on main, it
builds the bundle with Arakun's version, signs it with the upload key and uploads it to
a Google Play track (internal unless asked):

```sh
gh workflow run android-release.yml --ref main                    # internal track
gh workflow run android-release.yml --ref main -f track=beta -f status=draft
```

It needs the upload keystore and a Play service account in the `release` environment;
the workflow's header lists the secrets. Without the keystore it skips; without the
service account the signed bundle is the run's artifact. Play takes the app's first
bundle only by hand in Play Console, and `android/tools/play-upload.py` sends the ones
after it. To build a signed bundle yourself:

```sh
# Gradle reads ORG_GRADLE_PROJECT_<name> as -P<name>; this keeps the passwords out of
# the shell's history and the process list. The keystore path is from android/.
read -rs ORG_GRADLE_PROJECT_uploadStorePassword && export ORG_GRADLE_PROJECT_uploadStorePassword
read -rs ORG_GRADLE_PROJECT_uploadKeyPassword && export ORG_GRADLE_PROJECT_uploadKeyPassword
./gradlew :app:bundleRelease -PversionName=0.18.0 -PversionCode=2 \
  -PuploadStoreFile=upload.jks -PuploadKeyAlias=upload
```

## Crash reports

On first launch a release build asks whether to send crash reports to Arakun's PostHog
project, as the desktop does; the inbox's menu changes the answer later. Only uncaught
exceptions go: their type, message and stack trace, with URLs, paths, emails and
anything shaped like a secret taken out of the message (`Telemetry.kt`, the same rules
as the desktop's `src/telemetry.rs`). No usage events, screens, deep links or person
profiles. Debug builds send nothing; `-PposthogKey=` builds a release that never asks.
Release builds are shrunk but not obfuscated, so stack traces read without a mapping
file.

## Signing in

Tap "Sign in with GitHub". The app shows a code: copy it, open GitHub, and enter it
there (GitHub's device flow, as `arakun-server login` does). Sign in with the account
you use in Arakun on your computer. The app then lists the account's computers; with
only one it opens it straight away, and it opens the one you picked last on the next
launch. "All computers" in the inbox's menu goes back to the list.

The sign-in is a session on `app.arakun.dev`, named after this phone's model. The
inbox's menu has Account: the GitHub login, the operator's notices, the account's
signed-in devices (sign out any of them) and Sign out. Every signed-in device can reach
all the account's computers, so sign out any you do not know.

The session and the computer picked last are sealed with a key the Android Keystore
keeps on this device (`CredentialStore.kt`), never in plain preferences or logs; the
app's data is left out of backups and device transfers. The socket to the relay sends
the session in its `Authorization` header, and the app checks the computer's handshake
against the key the account has for it, after checking that key matches the computer's
label. A plain `http://` account host is only accepted on this machine
(`wrangler dev`), and only a debug build may speak plain http at all
(`src/debug/res/xml/network_security_config.xml`).

This app reaches your computers only by signing in. On Same Wi-Fi or over Tailscale, with
no account, the computer's own page does the pairing in the browser: scan the code in
Arakun's Phone tab with the camera, or open an `arakun://pair?link=` link to that page.
The app asks first ("Open <host> in the browser?"), since any app or web page can open
such a link, then hands it to the browser.

The relay's old pairing links and the ticket they gave are gone: the first launch after
the update deletes the stored ones, and an `arakun://pair?link=` link to the relay only
says "Relay links are no longer used. Sign in to reach your computers."

## What it does

- The inbox: every session, the ones waiting for you first, grouped by project.
- A session, live: messages, tool calls with their output, plans, thinking.
- Send a prompt (queued while the agent works), Stop, allow or deny a permission (a
  one-time allow is held, an allow for the session asks twice), answer the agent's
  questions, load older entries.
- Start a session in any place the computer offers.

Not yet: photos, diffs, session settings, plan limits, pairing on Same Wi-Fi or
Tailscale inside the app, and push notifications (the phone page's push is Web Push;
native push needs FCM support on the computer).
