# RBridger

> Бесплатная альтернатива [VBridger](https://store.steampowered.com/app/1898830/VBridger/) с открытым исходным кодом

Кросс-платформенный мост между источниками отслеживания лица и [VTube Studio](https://github.com/DenchiSoft/VTubeStudio).  
Поддерживает **iPhone** (полный набор ARKit блендшейпов через VTube Studio iOS) и **Веб-камеру** (нейросетевое ONNX-отслеживание).

[English documentation](readme.md)

---

## Скачать

Перейдите в [**Releases**](https://github.com/LakoMoor/RBridger/releases/latest):

| Платформа | Файл |
|-----------|------|
| macOS     | `RBridger-x.x.x-macos.dmg` |
| Linux     | `RBridger-x.x.x-linux-amd64.deb` |
| Windows   | `RBridger-x.x.x-windows-setup.exe` |

---

## Быстрый старт

1. Откройте **VTube Studio** на ПК — включите VTS API (Настройки → Основные → VTube Studio API, порт 8001).
2. Запустите **RBridger**.
3. Перейдите на вкладку **Config** → загрузите файл конфига или нажмите **Template**, чтобы начать с готового пресета `ovrog.json`.
4. Перейдите на вкладку **Bridge** → выберите источник (iPhone или Веб-камера) → нажмите **Connect**.
5. VTube Studio покажет всплывающее окно аутентификации — подтвердите его.
6. Строка статуса внизу показывает:
   - 🔴 **Disconnected** — мост не запущен
   - 🟡 **Connecting…** — мост активен, ожидание подтверждения в VTS
   - 🟢 **Connected** — данные отслеживания поступают в VTS

---

## Источники

### iPhone

Использует iOS-отслеживание VTube Studio по локальной Wi-Fi сети (ARKit, 50+ блендшейпов).

1. Откройте VTube Studio на iPhone.
2. В настройках iOS-приложения включите **"Отправлять данные на ПК"** и запомните показанный IP-адрес.
3. Введите этот IP в RBridger (вкладка Bridge) и нажмите **Connect**.

### Веб-камера

Нейросетевое отслеживание с помощью ONNX-моделей, которые автоматически загружаются при первом запуске (~3 МБ, сохраняются в `~/.rusty-bridge/`):
- **UltraFace RFB-320** — обнаружение лица
- **106-точечный MobileNetV1** — определение ориентиров лица

Веб-камера предоставляет меньше переменных, чем iPhone (повороты/позиция головы, моргание, челюсть, рот, брови). Полный набор ARKit блендшейпов доступен только через iPhone.

---

## Конфиг трансформаций

`.json`-файл, задающий правила преобразования сырых данных отслеживания в параметры VTube Studio с помощью математических выражений.

### Формат

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

| Поле           | Описание |
|----------------|----------|
| `name`         | Имя параметра VTube Studio. Встроенные параметры VTS используются повторно; неизвестные имена автоматически создают кастомные параметры. |
| `func`         | Математическое выражение, вычисляемое через [evalexpr](https://docs.rs/evalexpr). Доступны переменные из таблицы ниже. |
| `min` / `max`  | Диапазон значений параметра, передаваемый в VTube Studio. |
| `defaultValue` | Значение, используемое когда лицо не обнаружено. |

### Доступные переменные

#### Голова

| Переменная | Описание |
|------------|----------|
| `HeadRotX` | Наклон (кивок вверх/вниз) |
| `HeadRotY` | Поворот (влево/вправо) |
| `HeadRotZ` | Крен (наклон в сторону) |
| `HeadPosX` | Горизонтальное положение |
| `HeadPosY` | Вертикальное положение |
| `HeadPosZ` | Глубина (расстояние от камеры) |

#### Глаза

```
EyeBlinkLeft    EyeBlinkRight   — моргание
EyeWideLeft     EyeWideRight    — широко открытые глаза
EyeSquintLeft   EyeSquintRight  — прищур
```

#### Взгляд

```
EyeLookUpLeft     EyeLookUpRight
EyeLookDownLeft   EyeLookDownRight
EyeLookInLeft     EyeLookInRight
EyeLookOutLeft    EyeLookOutRight
```

#### Брови

```
BrowDownLeft     BrowDownRight    — нахмуренные брови
BrowInnerUp                       — подъём внутренних частей
BrowOuterUpLeft  BrowOuterUpRight — подъём внешних частей
```

#### Рот и челюсть

```
JawOpen   JawLeft   JawRight   JawForward
MouthClose
MouthSmileLeft    MouthSmileRight   — улыбка
MouthFrownLeft    MouthFrownRight   — недовольство
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

#### Прочее

```
CheekPuff                          — раздутые щёки
CheekSquintLeft   CheekSquintRight — прищур щёк
NoseSneerLeft     NoseSneerRight   — морщины на носу
TongueOut                          — высунутый язык
```

### Синтаксис выражений

Поле `func` использует синтаксис [evalexpr](https://docs.rs/evalexpr). Примеры:

```
-HeadRotY * 1.5
math::clamp(JawOpen * 2, 0, 1)
(EyeBlinkLeft + EyeBlinkRight) / 2
math::abs(MouthSmileLeft - MouthSmileRight)
```

Доступные функции: `math::abs`, `math::clamp`, `math::min`, `math::max`, `math::sin`, `math::cos`, `math::sqrt`, `math::floor`, `math::ceil`, `math::round`.

Переменные, которые не предоставляет текущий источник отслеживания (например, взгляд при использовании веб-камеры), автоматически принимают значение `0`.

### Редактор конфига

Вкладка **Config** содержит встроенный редактор:
- **Load / Save** — открыть или сохранить `.json`-файл
- **Template** — загрузить встроенный пресет `ovrog.json` (33 параметра, оптимизирован для iPhone)
- **＋ Add / Delete / Up / Down** — управление списком параметров
- Поле формулы многострочное; панель справочника переменных всегда видна справа

### Встроенный шаблон (ovrog.json)

Кнопка Template загружает конфиг из 33 параметров:
- Углы головы (X/Y/Z) с формулами коррекции перспективы
- Открытие/закрытие глаз, взгляд, моргание
- Открытие рта, улыбка, поджатие губ, воронка
- Подъём/опускание бровей
- Щёки, язык, нос

Основан на оригинальном `ovrog.json` из [rusty-bridge by ovROG](https://github.com/ovROG/rusty-bridge).

---

## Настройки

| Параметр | Описание |
|----------|----------|
| VTS Port | Порт VTube Studio API (по умолчанию: 8001) |
| iPhone IP | Локальный IP iOS-устройства |
| Webcam | Индекс используемой камеры |

---

## Сборка из исходного кода

```bash
git clone https://github.com/LakoMoor/RBridger.git
cd RBridger

# Только UI (без веб-камеры)
cargo build --release -p rbridger-ui

# UI с поддержкой веб-камеры (при первой сборке загружается ONNX Runtime ~100 МБ)
cargo build --release -p rbridger-ui --features webcam

# Бинарный файл: target/release/rbridger-ui
```

### Требования

- Rust 1.78+
- **macOS**: Xcode Command Line Tools
- **Linux**: `libgtk-3-dev libv4l-dev libudev-dev`
- **Windows**: MSVC toolchain

### Создание пакетов

```bash
# Linux .deb (запускать на Linux):
bash dist/linux/build_deb.sh

# Windows installer (запускать на Windows после cargo build):
cd dist/windows && makensis installer.nsi
```

Достаточно создать тег `v*` — [GitHub Actions](.github/workflows/release.yml) автоматически соберёт macOS DMG, Linux DEB и Windows installer.

---

## Структура проекта

```
rbridger/
├── ui/          # GUI-приложение (egui/eframe)
│   └── src/main.rs
├── vts/         # Основная библиотека
│   ├── src/vtspc.rs      # WebSocket-мост с VTube Studio
│   ├── src/vtsphone.rs   # UDP-трекер iPhone
│   └── src/webcam.rs     # ONNX-трекер веб-камеры
├── configs/     # Примеры и дефолтные конфиги
└── dist/        # Скрипты упаковки (deb, nsi)
```

---

## Благодарности

- Оригинальный проект: [rusty-bridge](https://github.com/ovROG/rusty-bridge) by ovROG
- Этот форк: [rbridger](https://github.com/LakoMoor/RBridger) by LakoMoor

---

## Лицензия

[GNU General Public License v3.0](LICENSE)
