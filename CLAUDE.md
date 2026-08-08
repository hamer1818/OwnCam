# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

OwnCam turns an Android phone into a Linux webcam over WiFi. Written because DroidCam
caps at 640x480/14fps and the alternatives are closed-source or need extra DKMS modules.
`telefon-kamera-plani.md` is the design plan (Turkish); `README.md` is the user-facing doc;
`YAPILACAKLAR.md` is the open-work list, including a section on what was **decided against**
— read that before proposing Phase 1, Windows support, or an inference library.

Comments, UI strings, and docs are Turkish, written without Turkish diacritics in Kotlin
source (`goruntu`, not `görüntü`). Match that style.

## Build and run

Android — the system JDK (26) does not work with AGP. Use Android Studio's bundled JBR:

```bash
cd android && JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleDebug
```

APK lands at `android/app/build/outputs/apk/debug/app-debug.apk`. There is no USB device
attached in this environment, so installing requires the user (or `adb` over WiFi if they
enable it). AGP 9 provides Kotlin support natively — do **not** add the
`org.jetbrains.kotlin.android` plugin, the build fails if you do.

Desktop app — Rust + egui, **Linux only**, supersedes `linux/owncam-desktop.py`
(the old GTK/Python UI, still in the tree but no longer the app):

```bash
cd desktop
cargo build --release          # target/release/owncam, 10.4 MB
cargo test                     # 52 tests
cargo test seg::plan           # one module
cargo test olcek_buyutme       # one test by name substring
```

The system toolchain (rustc 1.89) builds this; no nightly, no extra targets.

Windows is out of scope — no cross-compile, no DirectShow sink. Don't add them
back without being asked.

Running it without clicking through the UI (useful for end-to-end checks):

```bash
OWNCAM_EFFECT=bulanik ./target/release/owncam 192.168.1.105
# OWNCAM_EFFECT = kapali | bulanik | renk | foto
# OWNCAM_EFFECT_PHOTO=/path/to.jpg   OWNCAM_HOST=…   OWNCAM_DEVICE=/dev/videoN
```

There is one `#[ignore]`d test that writes composited frames to `/tmp` for
visual inspection — the automated tests cover numbers, not what it looks like:

```bash
OWNCAM_KARE=/tmp/frame.rgba OWNCAM_EN=1280 OWNCAM_BOY=720 \
  cargo test --release efekt_ornekleri -- --ignored --nocapture
```

See `desktop/README.md` for why egui, how the pluggable virtual-camera sink
works, and how background removal is implemented and verified. The decode path
is unchanged — ffmpeg stays a subprocess, because the Phase 0 measurements
already beat every target and no metric would improve.

Linux side is plain bash, no build step:

```bash
linux/install.sh
```

Copies scripts to `~/.local/bin` and the unit to `~/.config/systemd/user`. After editing a
script, re-run `install.sh` (or `install -m755` the single file) — the installed copy is
what `systemctl --user` runs.

The bash and Kotlin sides have no tests; verification there is measurement
(see Diagnostics). The Rust side is tested.

## Architecture

```
Camera2 ─> SurfaceTexture ─> GL ─┬─> MediaCodec ─> TCP :5299 ─> ffmpeg ─> /dev/video11
          (rotation matrix)      │   H.264 baseline                          │
                                 └─> on-screen preview                       ▼
                                                                     OBS ─> /dev/video10
```

With background removal on, the desktop app sits in the middle of the last
hop: `ffmpeg ─> full-res RGBA ─> GPU (segment + composite) ─> ffmpeg ─>
/dev/video11`. With it off, that hop is untouched — the effect costs nothing
when unused.

Four decisions drive most of the code:

**The phone is the TCP server, the PC is the client.** `ufw` is active with default-DROP
inbound. This direction means no firewall rule is ever needed on Linux. Reversing it breaks
the whole setup — this is why Iriun failed and DroidCam worked.

**The wire format is raw Annex-B, not length-prefixed.** MediaCodec already emits Annex-B,
and `ffmpeg -f h264 -i tcp://…` consumes it with zero Linux-side code. (The plan document
says "4-byte length prefix" in §5.1 and contradicts itself in §5.2 — the code follows §5.2.)

**Everything stays on the GPU.** Camera2 writes into a SurfaceTexture, GL draws that texture
twice — once into MediaCodec's input surface, once into the preview SurfaceView. Frames never
become byte arrays on the CPU. The GL pass exists only to apply rotation and to make the
preview show *exactly* what is being sent.

**Latency is defended by dropping frames, never by buffering.** The Android send queue holds
2 frames and drops the oldest when full (then requests a keyframe to recover). ffmpeg runs
with `nobuffer`, `low_delay`, `avioflags direct`, `max_delay 0`, `fps_mode passthrough`.

`probesize`/`analyzeduration` are the exception: the plan's `32`/`0` had to be raised to
`500000` in the Rust receiver. A `-vf scale` filter graph must know the frame size up front,
and with `probesize 32` ffmpeg exits with "unspecified size" before it can parse the SPS.
Those two values only affect the first moment of a connection, not steady-state latency.

### Background removal (`desktop/src/seg/`)

The segmentation network runs in **our own WGSL compute kernels** on a
headless wgpu device, not in an inference library. That was a measured
decision, not a preference: `tract` solves this model in **32 ms/frame** on
the CPU (the 30 fps budget is 33 ms) and adds 35 MB to the binary; the WGSL
kernels take **1.35 ms** and add 4.2 MB. The device is headless so processing
runs off the egui paint loop — minimising the window must not stall the
virtual camera.

The model is MediaPipe Selfie Segmentation (Apache-2.0, 462 KB ONNX). Two
non-obvious facts cost real debugging time and are encoded in tests:

- The ONNX build uses **half_pixel** `Resize`; the TFLite build of the *same*
  model uses asymmetric. They do not agree.
- `ConvTranspose` weights are `[C_in, C_out, kh, kw]` in ONNX and
  `[C_out, kh, kw, C_in]` in TFLite. The wrong layout yields a mask with
  vertical striping — visible, but with no obvious cause.

Correctness is not eyeballed. `desktop/tests/fixtures/` holds an input and the
mask produced by an independent ONNX runtime (`tract`); the GPU kernels are
measured against it (max error 0.0020, mean 0.00004, below the fixture's u8
quantisation floor of 1/255).

**If you change the model or the kernels, regenerate the fixture — don't relax
the tolerance.** The oracle is a throwaway crate outside the repo: `tract-onnx`
0.23, which needs a newer rustc than the system one (`rustup toolchain install
stable`), fed the same 256x256 input. Two things it will fight you on: clear
`graph.value_info` and the output types, and pin the symbolic `batch_size`
dimension to 1, or shape inference refuses to unify.

Getting here took a long detour worth not repeating: hand-implementing the
**TFLite** build of this model produced a plausible-looking but wrong mask, and
every unit test of every operator passed. Only an independent runtime found it.
If a mask looks subtly wrong, suspect an operator convention, not arithmetic.

**The model's scope is selfie framing** — head and shoulders with a
distinguishable background. On an extreme close-up (face filling the frame,
flat white wall behind) it returns a near-empty mask. That is the model's
limit, not a bug; do not go looking for one.

The composite writes **YUV420 directly**, not RGBA: readback drops from 4 to 1.5 bytes per
pixel and the second ffmpeg passes bytes through instead of converting. Measured A/B on the
same source: 15.5% → 9.7% total CPU. Each thread writes a whole `u32` so byte-level
read-modify-write cannot race, which requires plane lengths divisible by 4; `output_format`
falls back to RGBA when a frame size would break that.

There is no extra storage binding for it — the composite already uses the 8-buffer limit, so
YUV is written past the RGBA region of the same output buffer. The mask-coverage reduction
does the same trick, writing into a small scratch area at the end of the arena.

Coverage (the mask's mean) reaches the UI so it can say "kisi bulunamadi" instead of silently
producing a broken-looking effect. Measured: 0.56 head-and-shoulders, 0.0000 flat wall,
0.0013 on the extreme close-up; the warning threshold is 0.02.

Effects on/off switch the pipeline shape, so the receiver restarts; every
other effect setting applies live.

### Android (`android/app/src/main/java/com/owncam/`)

`StreamService` is the owner: foreground service, wake lock, `WIFI_MODE_FULL_HIGH_PERF` WiFi
lock, the `/status` command handlers, auto-rotation, and it wires `CameraEncoder` to
`TcpVideoServer`.

Auto-rotation uses an `OrientationEventListener` **in the service, not the Activity**, so the
image stays upright when the phone is turned with the app screen off. The measured rule is
`imageRotation = device orientation` — the listener already reports clockwise degrees, which
is exactly what the measurement wants (see Known issues; `SENSOR_ORIENTATION` plays no part).
Readings are snapped to 90° and debounced before they take effect.

`ORIENTATION_UNKNOWN` (phone lying flat) is **ignored, not treated as zero** — the last known
orientation is deliberately kept stale, because lying flat on a desk is exactly how a webcam
sits and there is no meaningful "up" to read.

`CameraEncoder` opens the camera, configures MediaCodec, and owns both frame sizes
(`captureSize` / `frameSize`). `gl/FrameRenderer` owns the GL thread and both output
surfaces; `gl/EglCore` and `gl/TextureRenderer` are the EGL/shader plumbing.

`MainActivity` builds its UI programmatically (no XML layouts) and polls
`StreamService`'s companion-object state every 700 ms.

`StatusServer` is a tiny HTTP endpoint on :5300 — see Diagnostics.

### Android 10 constraints

`minSdk = 29`. `MediaFormat.KEY_LOW_LATENCY` is API 30 and unavailable on the target phone.
Low latency instead comes from `KEY_PRIORITY = 0` (realtime) plus **Baseline profile** — no
B-frames means the encoder cannot accumulate frames. API 30+ keys are set behind a
`Build.VERSION.SDK_INT` guard. If `configure()` rejects the explicit profile/level, the code
retries without them.

### Linux (`linux/`)

`owncam-receive.sh` is the headless receiver: ffmpeg with the low-latency flags and
exponential-backoff reconnect. It finds the phone by shelling out to `owncam-discover.sh`,
which is the only remaining **avahi** dependency — the Rust app uses the `mdns-sd` crate and
needs no avahi.

Raw Annex-B carries no timestamps, so the input is tagged `-r $OWNCAM_FPS` — without it
ffmpeg emits ~30 `Non-monotonic DTS` warnings per second. `OWNCAM_FPS` must match the
phone's frame rate.

The bash receiver and the desktop app are alternatives, not layers: both write to the same
`/dev/video11`, so running them together makes one of them fail. Stop the systemd unit before
using the app.

## Rotation and frame geometry

**The camera always produces a landscape buffer and that cannot be changed.** The sensor is
fixed to the phone body, `SCALER_STREAM_CONFIGURATION_MAP` offers only landscape sizes, and
Camera2 never rotates. When the phone is mounted upright, that landscape buffer depicts a
*portrait* slice of the world. So "it comes out landscape" is a fact about the buffer's
shape, not about the framing — and no amount of code changes it.

The only real question is how a portrait-looking view should sit inside the frame. There are
exactly two sane answers, and `StreamConfig.FrameMode` names them. **Neither produces black
bars** — bars were the symptom of never having made this choice.

| Mode | Wire key | Frame | Behaviour |
|---|---|---|---|
| `FILL` (default) | `telefona-uy` | follows the phone's physical orientation: upright → 720x1280, sideways → 1280x720 | content covers the frame, overflow cropped |
| `FIT` | `tam-kadraj` | transposed at 90/270 → 720x1280 | nothing cropped, receiver gets portrait video |

The wire keys are duplicated in `desktop/src/app.rs` (`FRAME_MODES`) and must match
`StreamConfig.FrameMode` exactly — a test asserts the FILL key.

`CameraEncoder` keeps two sizes: `captureSize` (from the camera, always landscape) and
`frameSize` (what the encoder emits, per the table). `FrameRenderer.crop` picks `coverScale`
vs `fitScale` in `buildMatrix`; everything else about the GL path is unchanged.

Two non-obvious details:

- **FILL + 90/270 selects a 4:3 capture size, not 16:9.** The output width then comes from
  the camera's *short* axis, so a 16:9 request throws it away: source region 720x405 versus
  1080x608 from a 4:3 capture at the same output. `selectCaptureSize` maximises the short
  axis within `MAX_CAPTURE_AREA_FACTOR`.
- **FIT falls back gracefully.** If the encoder rejects the transposed size — many hardware
  encoders demand width ≡ 0 mod 16, and 1920x1080 transposed gives width 1080 — the code
  rounds down to the reported alignment (1072x1920, 99.3% coverage), and only then falls
  back to a pillarboxed landscape frame. `pillarboxed` is true only in that last case.

Rotation changes take the cheap path when they can: `CameraEncoder.applyOrientation(degrees,
device)` returns `true` and just updates the GL matrix when neither the quarter-turn parity
nor `frameSize` changes; on `false`, `StreamService.setRotation` restarts the stream.
`presetOrientation` seeds both values before `start()` so the first frame is already correct.

The rotation and the device orientation are **two separate inputs** and both are needed:
rotation orients the content, device orientation decides the frame's shape. Collapsing them
was the bug that produced portrait video in a landscape frame.

`config.mirror` flips the image horizontally, also in the GL matrix. It is written into the
matrix **first** so it lands in output space (after the rotation) — `scaleM` post-multiplies,
so the first call in code is the last operation applied. Mirroring before the rotation gives
a vertically flipped result at 90/270.

One trap, already sprung once: the GL layer is skipped entirely when rotation is 0 and
preview is off (camera feeds the encoder directly). Mirroring lives in GL, so that shortcut
also has to check `!config.mirror`.

The default is **off**: Camera2 hands back the true scene, so text on a shirt reads correctly
for the viewer even though it feels backwards to the person on camera. Which one is wanted
depends on use, so it is a setting rather than a constant.

There is exactly one rotation knob, `config.imageRotation`, and it is persisted. An earlier
design added a second `manualOffset` on top of it; the sum was untrackable and the three
status fields (`imageRotation` / `manualOffset` / `appliedRotation`) disagreed with each
other. `/rotate` and `/config?rotation=` now both write the same field.

**The angle is measured, not derived** — see Known issues.

## Diagnostics

The phone exposes its live state so the PC can read it without touching the phone:

```bash
owncam-status.sh              # summary table
owncam-status.sh --json
owncam-status.sh --rotate 90  # set and persist the rotation remotely
owncam-snapshot.sh            # grab one frame from /dev/video11 to a PNG
owncam-calibrate.sh           # sweep all four angles, contact sheet, save the pick
```

`owncam-snapshot.sh` matters: reading the PNG is how you check orientation and framing
yourself instead of asking the user. Stage counters in the status output
(`cameraFrames` → `glDraws` → `encoderOutputs` → `framesSent`) localize a stall to a
specific link in the chain.

Performance measurement (plan §9):

```bash
owncam-measure.sh 15
```

Measured baseline on this system, 1080p: 29.6 fps, 3 ms jitter, 0 gaps over 100 ms,
receiver at 6% of one core (720p) / 13% (1080p). Phase 0 exit criteria and the plan's
*final* targets were both met without building the Phase 1 Rust receiver — see the
"Neden burada duruyoruz" section of README.md before proposing to build it.

## Environment facts

- `/dev/video11` is the source device (phone → here), `/dev/video10` is OBS's virtual camera
  output. `/etc/modprobe.d/v4l2loopback.conf` defines both with `exclusive_caps=1`, so a
  device looks unreadable until a producer writes to it — that is normal, not a bug.
- A stale `ffmpeg` capture holding `/dev/video11` makes later opens fail with
  "Not a video capture device". Kill leftovers before diagnosing.
- Killing processes with `pkill -f owncam…` also matches the shell running the command.
  Resolve PIDs first, then `kill` by number.
- Reloading the v4l2loopback module requires OBS closed and the owncam service stopped;
  `relabel-v4l2loopback.sh` handles both.

## Known issues

**RESOLVED by measurement (2026-08-07). On the target phone the correct rotation is 0.**

Measured over adb with two independent ground truths at once: the accelerometer read
`(-0.95, 8.85, 3.44)` — +Y ≈ 1g, so the phone was physically upright in its natural portrait
orientation (`Display.getRotation()` = 0) — and the captured frame contained a person, giving
an unambiguous "up". The stream was at rotation 90 and needed a further 270° to stand
upright, so the correct value is `(90 + 270) % 360 = 0`.

**`SENSOR_ORIENTATION` on this device is not usable.** It reports 270, but the buffer is
already upright when the device is in its natural orientation. Every textbook formula
(CameraX `CameraOrientationUtil` and both variants) predicts 270 and is therefore wrong here
by a constant −270. This is why three separate derivation attempts failed; it was never an
algebra mistake.

Consequences, both of which were real bugs:

- Do not derive the rotation from `sensorOrientation`. It is kept in the status JSON for
  diagnostics only.
- **Do not derive the phone's physical mounting from `imageRotation` either.** Here upright
  mounting means rotation 0, so "rotation 0 ⇒ mounted landscape" is exactly backwards.
  `MainActivity` used that inference to lock the screen and forced landscape while the phone
  was held upright; it now uses `SCREEN_ORIENTATION_FULL_SENSOR` and lets the phone decide.

For a different device or mounting, `owncam-calibrate.sh` sweeps 0/90/180/270, grabs a frame
at each, builds a contact sheet, and saves the chosen angle. Point the camera at something
with a clear "up" — a person or a room, never the ceiling.

**The pipeline can wedge — but no longer reproduces.** Frame production once stopped at ~10
frames while the app process stayed alive. Suspected cause: the preview EGL surface is
vsync-locked, so `eglSwapBuffers` blocks indefinitely once the SurfaceView is destroyed
(app backgrounded / screen off), stalling the GL thread and starving the encoder.
`EglCore.setSwapInterval(0)` was added for this.

Retested **on the phone** (CLT-L09, Android 10) and it holds: 29.3 fps in the foreground,
28.3 backgrounded, 28.1 with the screen off, all three stages in lockstep, 0 dropped.

Verify the state transitions actually happened (`dumpsys window` for focus, `dumpsys power`
for `mWakefulness`) — the first attempt looked like a pass but the adb connection had
silently dropped, so the keyevents never reached the phone and the app never left the
foreground. Advancing counters proved nothing.

If it ever recurs, the stage counters (`cameraFrames` → `glDraws` → `encoderOutputs` →
`framesSent`) localize it.
