# Clipper: notes for agents and contributors

Rust + GPUI (gpui-kit) macOS screen recorder and screenshot editor. `cargo build`,
`cargo test` (needs ffmpeg/ffprobe on PATH), `scripts/bundle.sh` for the .app/.dmg.

## Where state lives

Read `docs/state-machines.md` before adding or changing state. Phases that exclude each
other are one enum (`Stage`, `timeline::Gesture`, `webcam::LivePreview`, `cam_bubble::Feed`,
`editor::view::RenderState`, …), never a set of booleans. Debug builds print
`state invariant broken: …` on stderr when `Stage` and its data disagree.

- `src/theme.rs`: every design token. Light and dark palettes (paper, mist, ink, ink-soft,
  stone, coral, success/warning/error) and the System/Light/Dark choice; type sizes
  (`T12`..`T32`, Figtree bundled), radii (`R_STAGE` 24, `R_PANEL` 16, `R_CONTROL` 8, pills are
  `rounded_full`), spacing (`S1`..`S6`), the one floating shadow, motion durations, and the
  content swatches (`tk::content`). Views never hold a hex value or a raw size. Selected =
  ink fill (`tk::selected()`) with paper text; coral only for Record, Export and the recording
  indicator. Surfaces on the user's pixels (region selector, screen picker, video overlays)
  use `tk::DARK` or `tk::scrim`.
- `src/ui.rs`: the shared components (buttons incl. `coral_button`, `pill_tabs`, `segmented`,
  `chip`, `swatch`, `popover`/`menu`, `toast_pill`, `dialog`, `focusable` with the focus ring).
- `src/home.rs`: the Library (start screen): scans recordings, exports and screenshots,
  background thumbnails cached in `~/Library/Caches/Clipper/thumbs`, virtualized masonry.
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
| `CLIPPER_PANEL=<scene\|layout\|zoom\|captions\|output>` | Open the recording setup on a settings tab. |
| `CLIPPER_STUDIO=1` | Open the recording setup instead of the Library. |
| `CLIPPER_SIZE=WxH` | Initial window size (clamped to the 1100x680 minimum). |
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
