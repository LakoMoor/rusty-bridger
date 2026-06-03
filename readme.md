# RBridger

> Free, open-source alternative to [VBridger](https://store.steampowered.com/app/1898830/VBridger/)

Cross-platform bridge between face tracking sources and [VTube Studio](https://github.com/DenchiSoft/VTubeStudio).  
Supports **iPhone** (full ARKit blend shapes via VTube Studio iOS app) and **Webcam** (neural ONNX tracking).

[Русская документация](README_RU.md)

---

## Download

Go to [**Releases**](https://github.com/LakoMoor/RBridger/releases/latest):

| Platform | File |
|----------|------|
| macOS    | `RBridger-x.x.x-macos.dmg` |
| Linux    | `RBridger-x.x.x-linux-amd64.deb` |
| Windows  | `RBridger-x.x.x-windows-setup.exe` |

---

## Quick Start

1. Open **VTube Studio** on PC — enable the VTS API (Settings → General → VTube Studio API, port 8001).
2. Launch **RBridger**.
3. Go to the **Config** tab → load a config file or click **Template** to start from the built-in `ovrog.json` preset.
4. Go to the **Bridge** tab → choose source (iPhone or Webcam) → press **Connect**.
5. VTube Studio will show an authentication popup — accept it.
6. The status bar at the bottom shows:
   - 🔴 **Disconnected** — not running
   - 🟡 **Connecting…** — bridge active, waiting for VTS auth
   - 🟢 **Connected** — tracking data flowing into VTS

---

## Sources

### iPhone

Uses VTube Studio's iOS face tracking over local Wi-Fi (ARKit, 50+ blend shapes).

1. Open VTube Studio on your iPhone.
2. In the iOS app settings, enable **"Send data to PC"** and note the IP address shown.
3. Enter that IP in RBridger's Bridge tab and press **Connect**.

### Webcam

Neural tracking using ONNX models downloaded automatically on first run (~3 MB total, saved to `~/.rusty-bridge/`):
- **UltraFace RFB-320** — face detection
- **106-point MobileNetV1** — facial landmark detection

Webcam provides a subset of variables compared to iPhone (head rotation/position, eye blink, jaw, mouth, brows). Full ARKit blend shapes require iPhone.

---

## Transform Config

A `.json` file that maps raw tracking values to VTube Studio parameters via math expressions.

### Format

```json
[
  {
    "name": "FaceAngleY",
    "func": "-HeadRotY * 1",
    "min": -40.0,
    "max": 40.0,
    "defaultValue": 0
  }
]
```

| Field          | Description |
|----------------|-------------|
| `name`         | VTube Studio parameter name. Built-in VTS params are reused; unknown names create custom params automatically. |
| `func`         | Math expression evaluated with [evalexpr](https://docs.rs/evalexpr). Variables from the table below are available. |
| `min` / `max`  | Parameter value range passed to VTube Studio. |
| `defaultValue` | Value used when no face is detected. |

### Available variables

#### Head

| Variable | Description |
|----------|-------------|
| `HeadRotX` | Pitch (nodding up/down) |
| `HeadRotY` | Yaw (turning left/right) |
| `HeadRotZ` | Roll (tilting left/right) |
| `HeadPosX` | Horizontal position |
| `HeadPosY` | Vertical position |
| `HeadPosZ` | Depth (distance from camera) |

#### Eyes

```
EyeBlinkLeft    EyeBlinkRight
EyeWideLeft     EyeWideRight
EyeSquintLeft   EyeSquintRight
```

#### Gaze

```
EyeLookUpLeft     EyeLookUpRight
EyeLookDownLeft   EyeLookDownRight
EyeLookInLeft     EyeLookInRight
EyeLookOutLeft    EyeLookOutRight
```

#### Brows

```
BrowDownLeft     BrowDownRight
BrowInnerUp
BrowOuterUpLeft  BrowOuterUpRight
```

#### Mouth & Jaw

```
JawOpen   JawLeft   JawRight   JawForward
MouthClose
MouthSmileLeft    MouthSmileRight
MouthFrownLeft    MouthFrownRight
MouthLeft         MouthRight
MouthUpperUpLeft  MouthUpperUpRight
MouthLowerDownLeft  MouthLowerDownRight
MouthPressLeft    MouthPressRight
MouthStretchLeft  MouthStretchRight
MouthDimpleLeft   MouthDimpleRight
MouthRollUpper    MouthRollLower
MouthShrugUpper   MouthShrugLower
MouthPucker       MouthFunnel
```

#### Other

```
CheekPuff
CheekSquintLeft   CheekSquintRight
NoseSneerLeft     NoseSneerRight
TongueOut
```

### Expression syntax

`func` uses [evalexpr](https://docs.rs/evalexpr) syntax. Examples:

```
-HeadRotY * 1.5
math::clamp(JawOpen * 2, 0, 1)
(EyeBlinkLeft + EyeBlinkRight) / 2
math::abs(MouthSmileLeft - MouthSmileRight)
```

Math functions: `math::abs`, `math::clamp`, `math::min`, `math::max`, `math::sin`, `math::cos`, `math::sqrt`, `math::floor`, `math::ceil`, `math::round`.

Variables not provided by the current tracking source (e.g. gaze variables when using webcam) silently evaluate to `0`.

### Config editor

The **Config** tab has a built-in editor:
- **Load / Save** — open or save a `.json` file
- **Template** — load the built-in `ovrog.json` preset (33 params, iPhone-optimised)
- **＋ Add / Delete / Up / Down** — manage the parameter list
- The formula field is multi-line; the variable reference panel is always visible on the right

### Built-in template (ovrog.json)

The Template button loads a 33-parameter config covering:
- Face angles (X/Y/Z) with perspective correction formulas
- Eye open/close, gaze, blink
- Mouth open, smile, pucker, funnel
- Eyebrow raise/lower
- Cheek, tongue, nose

Based on the original `ovrog.json` from [rusty-bridge by ovROG](https://github.com/ovROG/rusty-bridge).

---

## Settings

| Setting | Description |
|---------|-------------|
| VTS Port | VTube Studio API port (default: 8001) |
| iPhone IP | Local IP of the iOS device |
| Webcam | Camera index to use |

---

## Building from source

```bash
git clone https://github.com/LakoMoor/RBridger.git
cd RBridger

# UI only (no webcam)
cargo build --release -p rbridger-ui

# UI with webcam support (downloads ONNX runtime ~100 MB on first build)
cargo build --release -p rbridger-ui --features webcam

# Binary: target/release/rbridger-ui
```

### Requirements

- Rust 1.78+
- **macOS**: Xcode Command Line Tools
- **Linux**: `libgtk-3-dev libv4l-dev libudev-dev`
- **Windows**: MSVC toolchain

### Creating packages

```bash
# Linux .deb (run on Linux):
bash dist/linux/build_deb.sh

# Windows installer (run on Windows after cargo build):
cd dist/windows && makensis installer.nsi
```

Push a `v*` tag to trigger the [GitHub Actions release workflow](.github/workflows/release.yml), which builds macOS DMG, Linux DEB, and Windows installer automatically.

---

## Project structure

```
rbridger/
├── ui/          # GUI application (egui/eframe)
│   └── src/main.rs
├── vts/         # Core library
│   ├── src/vtspc.rs      # VTube Studio WebSocket bridge
│   ├── src/vtsphone.rs   # iPhone UDP tracker
│   └── src/webcam.rs     # Webcam ONNX tracker
├── configs/     # Example and default configs
└── dist/        # Packaging scripts (deb, nsi)
```

---

## Credits

- Original project: [rusty-bridge](https://github.com/ovROG/rusty-bridge) by ovROG
- This fork: [rbridger](https://github.com/LakoMoor/RBridger) by LakoMoor

---

## License

[GNU General Public License v3.0](LICENSE)
