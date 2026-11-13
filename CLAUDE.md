# Clipper: notes for agents and contributors

Rust + GPUI (gpui-kit) macOS screen recorder and screenshot editor. `cargo build`,
`cargo test` (needs ffmpeg/ffprobe on PATH), `scripts/bundle.sh` for the .app/.dmg.

## Where state lives

Read `docs/state-machines.md` before adding or changing state. Phases that exclude each
other are one enum (`Stage`, `timeline::Gesture`, `webcam::LivePreview`, `cam_bubble::Feed`,
`editor::view::RenderState`, …), never a set of booleans. Debug builds print
`state invariant broken: …` on stderr when `Stage` and its data disagree.

- `src/theme.rs`: light and dark palettes and the Auto/Light/Dark choice. Views take every
  color from a semantic token (`tk::text()`, `tk::card()`, `tk::ink(alpha)` for overlays,
  `tk::green_ink()` for green as text/outline); never a raw hex. Surfaces that always sit on
  dark pixels (region selector, screen picker, video overlays) use `tk::DARK` directly.
- `src/editing/`: trim/split/cut logic (pure, unit tested); everything downstream reads
  raw time through `EditList::source_to_output` / `output_to_source`.
- `src/cutting.rs`: the edit list wired into the app (undo, derived preview data).
- `src/timeline.rs`: timeline UI and pointer gestures. `src/export.rs` + `src/composite.rs`:
  the ffmpeg render. `src/editor/`: the screenshot/image editor (`doc` model, `render`
  shared by canvas and export, `spline` arrow geometry, `merge` board layout; pure and
  unit tested; `view`/`panel` are the window).

## Driving the app without clicking (debug builds only)

The main window takes focus on launch; `CLIPPER_NO_ACTIVATE=1` opens it without stealing
focus (a covered window doesn't redraw, so screenshot it while visible).

| Variable | Effect |
|---|---|
| `CLIPPER_OPEN=<session dir>` | Open a saved recording (raw.mp4, cursor.csv, webcam.mp4, captions.json) in the editor. |
| `CLIPPER_EDIT_DEMO=1` | With `CLIPPER_OPEN`: apply a few splits and a cut. |
| `CLIPPER_PANEL=<layout\|scene\|zoom\|captions\|output\|shots>` | Open on a settings tab. |
| `CLIPPER_BG=<gradient\|animated\|image\|color>` | Pick a background type. |
| `CLIPPER_RATIO=16:9`, `CLIPPER_PLACEMENT=<circle\|square\|split>`, `CLIPPER_SPLIT=<0..1>`, `CLIPPER_FAKE_CAM=1` | Layout and a test-card webcam without a camera. |
| `CLIPPER_CAPTIONS=1`, `CLIPPER_CAP_MODEL=<id>`, `CLIPPER_FAKE_CAPTIONS=<session dir>` | Captions on, model, a long sample transcript (not saved). |
| `CLIPPER_SELECT=1` | Open the region selector. |
| `CLIPPER_DEMO=1` (+ `CLIPPER_EXPORT=1`) | Record ~9 s of the screen, pause/mute/stop, optionally open Export. Records the real screen. |
| `CLIPPER_SHOT=<full\|region\|window>` | Take a screenshot at launch. |
| `CLIPPER_EDIT=<image>`, `CLIPPER_EDIT_TOOL`, `CLIPPER_EDIT_SEL` | Open the image editor on a file, with a tool or selection. |
| `CLIPPER_MERGE=a.png,b.png` (+ `CLIPPER_MERGE_DIR=<dir>`) | Merge pictures into a new board and open it (in that folder, else the screenshots folder). |
| `CLIPPER_PRESET=<compare\|side\|single>` | Apply a merge preset when the editor opens. |
| `CLIPPER_BAR_CAPTURABLE=1` | Let screenshots capture the control bar and selector (normally hidden from capture). |
| `CLIPPER_KEEP=<dir>` | Tests keep their rendered output. |
| `CLIPPER_THEME=<light\|dark\|system>` | Override the saved appearance for this run. |

Copy a session to a scratch folder before opening it: the editor writes `edits.json` and
`preview/` next to the recording (the raw files are never modified).
