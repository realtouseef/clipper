# Recordly teardown (v1.4.0) → Clipper roadmap

Source: `/Applications/Recordly.app` (bundle id `dev.recordly.app`), analysed with
REA (`rea-agents analyze-javascript-application --integrity-policy record-and-continue`)
plus direct reading of the extracted `app.asar`.

> **License note.** Recordly is AGPLv3 with extra terms: no use of the "Recordly"
> name/branding, and code *derived* from it must credit Recordly in the UI and repo.
> Clipper reimplements behaviour in Rust/GPUI from scratch — no Recordly code, assets,
> name or cursor art is copied. Keep it that way, or add the attribution.

## Architecture

| Layer | Recordly | Clipper today |
|---|---|---|
| Shell | Electron + React (Vite), PixiJS (WebGL/WebGPU) preview | GPUI + gpui-kit |
| Capture (macOS) | `recordly-screencapturekit-helper` (native ScreenCaptureKit), ffmpeg fallback | ffmpeg avfoundation |
| Cursor | `recordly-native-cursor-monitor` (position, click type, **cursor shape**), `recordly-system-cursors` (exports system cursor images), uiohook for global input | `cursor.csv` (pos + clicks) |
| Windows | `recordly-window-list` (window capture targets) | display only |
| Captions | bundled `whisper-cli` + downloadable whisper-small model | — |
| Export | WebCodecs in renderer → frames streamed to main → ffmpeg mux; GIF via gif.worker | ffmpeg crop filter |
| Project | `.recordly` file (JSON editor state + media paths) | — |

IPC surface: 137 literal channels, 136 `electronAPI` members (one context bridge).

## Windows / screens

1. **HUD launch bar** — floating pill at bottom of screen (radius 20, padding 11×20, 40×40 icon buttons radius 11, 1px separators 26 tall). Contents: source picker (Screens / Windows tabs with thumbnails), mic picker, system-audio toggle, webcam picker + floating preview toggle, countdown delay (none/3/5/10), Record button, "More" menu (recordings path, open video, open project, hide HUD from recording, close). Popover menus: 300 wide, radius 16, blurred, uppercase 9px section labels, 8px-radius items.
2. **Recording state** of the same bar: REC/PAUSED + timer, Pause/Resume, Stop, mic toggle. HUD can be excluded from capture (capture protection) — ScreenCaptureKit honours it, unlike AVFoundation.
3. **Countdown overlay** (full-screen number).
4. **Editor window** — opens automatically after Stop.
5. **Source selector**, **update toast**, **project browser** dialogs.

## Editor

Layout: header (project name, unsaved dot, Undo/Redo, Save, Export) · centre preview
canvas · right settings panel with tabs · bottom timeline with tracks.

**Settings tabs:** Scene · Cursor · Zoom · Frame/Crop · Captions · Webcam · Audio.

- **Scene/background:** wallpaper presets, solid color, gradient, uploaded image, background blur, padding (linked/unlinked T/B/L/R), border radius ("Roundness"), shadow intensity, aspect ratio (16:9, 9:16, 1:1, 4:3, 4:5 …).
- **Cursor:** show/hide, loop cursor, size, style (several skins), smoothing (spring stiffness/damping/mass), motion blur, click bounce (+speed), sway, click effects (Ripple / Spotlight / Echo; color, size, opacity, duration). Cursor is re-rendered from telemetry (the real cursor is hidden in capture), so it can be enlarged and smoothed.
- **Zoom:** depth level per region, Auto (follows cursor) vs Manual (fixed focus point), zoom-in/out duration + easing (Glide / Smooth / Snappy / Linear), in-overlap, "Connect zooms" (pan between adjacent zooms instead of out-and-in; gap + duration + curve), zoom motion blur (temporal, samples, shutter), auto-apply suggested zooms to fresh recordings.
- **Crop:** drag-handle crop editor on the source video.
- **Captions:** local Whisper transcription, font/size/rows, animation (Fade/Rise/Pop), bottom offset, max width, box radius/opacity, split/merge.
- **Webcam overlay:** size, crop, roundness, shadow, mirror, "reacts to zoom", background removal.
- **Audio:** per-clip volume, normalize, system vs mic source, separate audio from clip.

**Timeline tracks:** clip, zoom regions (Z), trim regions (T — removed spans), speed regions (S), annotations (A: text / image / arrow / blur), captions, audio. Split clip (C), keyframes, Tab cycles overlapping items, timeline pan/zoom.

**Export:** MP4 (quality Low/Med/High/Original, frame rate) or GIF (fps, size preset, loop). Progress dialog → "Show in Folder".

## Auto-zoom algorithm ("Suggest Zooms from Cursor")

Telemetry is normalised (`cx, cy ∈ [0,1]`) with interaction types (move, click, double-click, mouseup, …).

1. **Classify each click** by what follows it:
   - double-click → `double-click-like` (strength 1500)
   - mouseup ≥200 ms later, moved >0.03 mostly horizontally (>1.8× vertical) → `text-selection` (1300)
   - <3 move samples in the next 2 s, or max drift <0.02 → `text-field-click` (1100)
   - cursor then goes down >0.03 mostly vertically → `dropdown-open` (1200)
   - otherwise `click-like` (900)
2. **Dwell heuristics:** split samples wherever consecutive moves exceed 0.02; a still run lasting 450–2600 ms becomes a candidate (strength = duration). Two weak candidates ≤450 ms and ≤0.035 apart merge into a double-click-like one.
3. **Only explicit (click) candidates generate zooms.** Sort by time, merge any within **2500 ms** of the previous; focus = strongest candidate's point.
4. Each cluster → zoom region `[first − 500 ms, last + 500 ms]`, clamped, skipped if it overlaps an existing region.
5. Easing used for camera motion includes ease-out-quint `1 − (1 − t)^5`.

## Gap vs Clipper

Clipper already has: record + pause/mute, floating control bar, global hotkeys, cursor log, reactive follow camera, ratio switching, ffmpeg export.

Missing, in suggested order:

1. **Editor shell** — header / preview / right panel / timeline layout (gpui-kit, white, squircle — per Clipper style).
2. **Scene styling** — background (color/gradient/wallpaper), padding, roundness, shadow around the video inside the output frame. Needs a compositing export path (ffmpeg filtergraph or a GPU renderer) instead of a plain crop.
3. **Zoom regions on a timeline** — auto-suggested from clicks (algorithm above), editable, Auto/Manual focus, eased in/out, connected zooms. Coexists with Clipper's reactive follow camera inside a zoom.
4. **Rendered cursor** — hide the OS cursor in capture, draw a scalable smoothed cursor + click ripple from `cursor.csv`.
5. **Trim + speed regions.**
6. **HUD launcher** before recording — source (screen/window), mic, countdown.
7. Later: ScreenCaptureKit capture (would let the bar be excluded natively), webcam overlay, annotations, captions (whisper), GIF export, project save/load.
