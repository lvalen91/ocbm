# CallSim — fake phone calls for Android Auto telephony testing

Self-managed Telecom `ConnectionService` (`PhoneAccount` with `CAPABILITY_SELF_MANAGED`).
Telecom, the dialer and Android Auto (gearhead) treat its calls as real calls: call screen,
answer/hang-up, audio routing including Bluetooth HFP/SCO. No SIM / cellular service needed.
Everything is driven from adb; the phone never needs to be touched.

Package `com.carlink.callsim`, minSdk 31, no third-party dependencies. Logcat tag: `CallSim`.

## Build

```sh
cd host/CallSim
~/.claude/bin/bt ./gradlew assembleDebug
# APK: ~/.cache/gradle-builds/callsim/app/outputs/apk/debug/app-debug.apk
```

`gradlew` and `gradle/` are symlinks into `../CarlinkAndroid` (same Gradle 9.6.1 wrapper,
AGP 9.2.1 with built-in Kotlin, JDK 21 daemon toolchain). Build output goes to
`~/.cache/gradle-builds/callsim` for the same iCloud-sync reason as CarlinkAndroid.

## Install and grant

```sh
adb install -r ~/.cache/gradle-builds/callsim/app/outputs/apk/debug/app-debug.apk
adb shell pm grant com.carlink.callsim android.permission.RECORD_AUDIO
adb shell pm grant com.carlink.callsim android.permission.POST_NOTIFICATIONS
adb shell pm grant com.carlink.callsim android.permission.BLUETOOTH_CONNECT   # only for the BT device name in logs
adb shell pm grant com.carlink.callsim android.permission.READ_MEDIA_AUDIO    # only for the --es wav override
adb shell appops set com.carlink.callsim USE_FULL_SCREEN_INTENT allow          # lock-screen incoming UI (Android 14+)
# Launch once: leaves the "stopped" state (manifest receivers get broadcasts), registers the
# PhoneAccount, and puts the app in the foreground so the first FGS start is unrestricted.
adb shell am start -n com.carlink.callsim/.MainActivity
adb logcat -s CallSim
```

`MANAGE_OWN_CALLS` and the `FOREGROUND_SERVICE_*` permissions are install-time; nothing to grant.

**PhoneAccount enablement:** none needed. Self-managed accounts are registered by the app
(`Accounts.register`, run on every process start and before every command) and are not
user-enableable; `TelecomManager.getPhoneAccount()` reports them enabled. Check with
`adb shell dumpsys telecom | grep -A3 callsim`. The account carries
`EXTRA_ADD_SELF_MANAGED_CALLS_TO_INCALLSERVICE=true` (dialer shows the call) and
`EXTRA_LOG_SELF_MANAGED_CALLS=true` (call log). Android Auto's InCallService opts into
self-managed calls on its own.

## Commands

All broadcasts target the receiver explicitly (`-n`); an action-only implicit broadcast would
not reach a manifest receiver on API 26+.

```sh
R="am broadcast -n com.carlink.callsim/.AdbReceiver"

# Incoming call: rings with the system ringtone; answer on the phone, on the AA screen,
# in the notification, or with ANSWER below.
adb shell $R -a com.carlink.callsim.INCOMING --es name "Test Caller" --es number "+15550100"
adb shell $R -a com.carlink.callsim.ANSWER
adb shell $R -a com.carlink.callsim.REJECT                     # decline while ringing

# Outgoing call via the CallSim account; far end auto-answers after 3 s (state ACTIVE).
adb shell $R -a com.carlink.callsim.OUTGOING --es number "+15550100"

# Hang up. Default = simulated far-end hangup (DisconnectCause REMOTE); --es cause local = LOCAL.
adb shell $R -a com.carlink.callsim.HANGUP
adb shell $R -a com.carlink.callsim.HANGUP --es cause local

# Extras
adb shell $R -a com.carlink.callsim.HOLD
adb shell $R -a com.carlink.callsim.UNHOLD
adb shell $R -a com.carlink.callsim.ROUTE --es route bluetooth   # earpiece | speaker | bluetooth | wired
adb shell $R -a com.carlink.callsim.STATUS                       # account, call state, audio mode, comm device, missing perms

# Far-end audio override (any 16-bit PCM WAV, mono or stereo, any rate):
adb push my.wav /sdcard/Download/my.wav
adb shell $R -a com.carlink.callsim.INCOMING --es name "WAV" --es number "+15550100" --es wav /sdcard/Download/my.wav
```

## Audio

While the call is ACTIVE:

* **Downlink (far end):** `assets/farend_16k.wav` loops into an `AudioTrack` with
  `USAGE_VOICE_COMMUNICATION` / `CONTENT_TYPE_SPEECH`. The connection is marked VoIP, so
  Telecom sets `MODE_IN_COMMUNICATION`, and playback follows Telecom's route: SCO when the HFP
  device owns the call, otherwise earpiece/speaker. The routed output device is logged on every
  change and every 5 s.
* **Uplink:** `AudioRecord(VOICE_COMMUNICATION, 16 kHz mono 16-bit)` writes
  `/sdcard/Download/callsim_uplink_<yyyyMMdd_HHmmss>.wav` and logs RMS/peak once per second;
  three consecutive near-silent seconds are flagged `** SILENT UPLINK **`.

```sh
adb shell ls /sdcard/Download/callsim_uplink_*.wav
adb pull /sdcard/Download/callsim_uplink_20260903_223000.wav .
```

If the Download directory cannot be written the file lands in
`/sdcard/Android/data/com.carlink.callsim/files/` (logged as `uplink recording start -> …`).

### Test pattern (`tools/gen_farend_wav.py`, deterministic, stdlib only)

30 s, 16 kHz, mono: five 6-second blocks. Each block = *N* × 1 kHz bursts (N = block index
1…5, so you can hear where in the file playback is) → DTMF `1234567890` → 400/600 Hz
alternation → 0.5 s silence. Regenerate with
`python3 tools/gen_farend_wav.py app/src/main/assets/farend_16k.wav` (byte-identical output).

## What the log shows

```
Connection created incoming=true name="Test Caller" number=+15550100 farEndWav=<assets/farend_16k.wav>
state -> RINGING
onShowIncomingCallUi (...)                 <- Telecom asks the app to ring; self-managed calls are not rung by Telecom
ringtone start uri=... playing=true
FGS started type=phoneCall / FGS type upgraded to phoneCall|microphone
answer from Telecom/InCallService          <- phone UI / AA screen / notification / adb
state -> ACTIVE
engine start: audioMode=3 (3=MODE_IN_COMMUNICATION)
far-end playback start src=assets/farend_16k.wav 16000 Hz 1ch 30 s ...
onCallAudioStateChanged route=BLUETOOTH supported=EARPIECE, BLUETOOTH, SPEAKER muted=false activeBtDevice=<name>
onCallEndpointChanged BLUETOOTH "<name>"
far-end routed device -> BLUETOOTH_SCO("<name>" id=..)
uplink recording start -> /sdcard/Download/callsim_uplink_....wav routed=BLUETOOTH_SCO(...)
uplink t=1s rms=812 (-32.1 dBFS) peak=4120
...
simulated far-end hangup from adb
state -> DISCONNECTED
uplink recording stopped: ... bytes ... s
```

Errors from Telecom surface as `onCreateIncomingConnectionFailed` / `onCreateOutgoingConnectionFailed`
(another call in progress, or `isIncomingCallPermitted()==false`) and as caught exceptions
from `addNewIncomingCall` / `placeCall`.

## Caveats

* The receiver is exported without a permission check (any app could fake a call). Test tool;
  uninstall when done.
* Foreground-service start from a pure-adb broadcast can be refused on Android 12+
  (`ForegroundServiceStartNotAllowedException`, logged). The CallStyle notification is then
  posted directly and the call proceeds; Telecom's binding keeps the process alive. If the
  uplink RMS is then flagged silent, run `adb shell am start -n com.carlink.callsim/.MainActivity`
  before the next call so the FGS (and its `microphone` type) can start from the foreground.
* READ_PHONE_STATE is not declared: self-managed calls do not need it.
* One call at a time. INCOMING/OUTGOING are refused while a call exists.

## Notification test (added 2026-09-04)

```
adb shell am broadcast -n com.carlink.callsim/.AdbReceiver -a com.carlink.callsim.NOTIFY --es from "Ann" --es text "Running late"
```

Posts a `MessagingStyle` notification on a HIGH-importance channel with the default notification
sound and reply / mark-as-read semantic actions — the shape Android Auto shows on the car screen.
Used to learn whether the chime is projected (system audio sink) or stays on the phone; a
shell-posted notification (`cmd notification post`) has no sound channel and cannot answer that.

