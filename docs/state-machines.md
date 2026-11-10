# State machines

Every piece of Clipper state that moves through phases, where it lives, and how it's
encoded. The rule: **one enum when the phases are exclusive**, so impossible combinations
can't be written down; separate fields only when they truly vary independently.

When you add state, check this list first. If a new `bool`/`Option` only makes sense
together with another one, it's a state machine: make it an enum (or one `Option` of a
struct) and add it here.

## Explicit (enums)

| Machine | Type | States | Notes |
|---|---|---|---|
| App lifecycle | `Stage` (`src/main.rs`) | Idle → Starting → Recording → Processing → Review ⇄ Exporting, Error | Transitions and invariants are documented on the type. Debug builds log broken invariants (`Clipper::stage_violations`). |
| Pointer gesture (main window) | `timeline::Gesture` | None, Seek, Zoom, Trim, Cam, Knob | One gesture at a time. `Trim` carries the edit from before the drag, which becomes a single undo step on release (`finish_gesture`). `holds_playback()` pauses preview playback and audio. |
| Idle webcam preview | `webcam::LivePreview` | Off, Running(Live), Failed | `Failed` sticks (no retry loop against a denied camera) until the device or switch changes (`restart_live`). Stopped whenever the window is closed. |
| Recording webcam bubble | `cam_bubble::Feed` | Waiting{until}, Running{live, since} | Retries a failed or frozen feed; waits after the webcam is switched back on. |
| Caption being typed | `captions_panel::CaptionEdit` (in an `Option`) | none / editing line i | The text field and the subscription that saves it on Enter or blur live and die together. |
| Export in progress | `export_dialog::Running` (in `Option`) | none / running {job, formats} | Set exactly while `Stage::Exporting`. |
| Editor canvas rendering | `editor::view::RenderState` | Idle, Running{draft, again} | One background render at a time; changes during a render queue one more; a draft (during a drag) is redone at full resolution after. Unit tested. |
| Video edit | `editing::EditList` + `editing::History` | kept segments; undo/redo stacks | Pure logic, unit tested. Saved as `edits.json` next to the recording. |
| Frame decoding | `frames::FrameCache` | want list → decoded frames | Background worker; the UI lists the frames it wants, most urgent first. |
| Region selector, editor drags | `selector::Drag`, `editor::view::Drag` | per-gesture enums | Already explicit. |

## Independent on purpose (left as separate fields)

| State | Why it's not one machine |
|---|---|
| Model download (`download`) and transcription (`stt`) | They run at the same time (you can download another model while transcribing). `cap_error` is the shared last-error slot. |
| Export estimate (`estimate`) and estimating (`estimating`) | Both at once is meaningful: the old sizes stay visible while new ones are worked out. |
| `playing` and `audio_restart` | `audio_restart` is a one-shot "restart audio at the playhead on the next tick" signal, not a phase. |
| `shot_busy` | A single busy flag across the screenshot steps (delay, selection, capture). |
| `confirm_reset` | A single "asked, waiting for the second click" flag. |
| Menu bar presence (`menubar::window_hidden`) | A global fact read by the camera, microphone meter and animation code to stay off while the window is closed. |
| Selections (`selected` zoom, `edit_sel` segment, `cap_selected`) | Different lists; clearing one doesn't imply the others. |

## Checking

- `cargo test` covers the pure machines (`editing`, `RenderState`, frame cache, captions remapping).
- Run a debug build (`cargo run`) and watch stderr for `state invariant broken:` lines.
