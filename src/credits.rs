//! Credits: the open-source data, fonts and icons bundled in the app, with their licenses.

use crate::ui;
use crate::{BG, MUTED, TEXT};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::prelude::*;
use gpui_kit::{App, Bounds, FontWeight, TitlebarOptions, Window, WindowBounds, WindowOptions, div, point, px, rgb, size};

struct Entry {
    name: &'static str,
    what: &'static str,
    license: &'static str,
}

const ENTRIES: [Entry; 6] = [
    Entry { name: "FFmpeg", what: "Video encoding (the bundled ffmpeg and ffprobe programs), GNU GPL v3; source links below", license: include_str!("../assets/licenses/FFmpeg.txt") },
    Entry { name: "uiGradients", what: "Gradient presets (github.com/ghosh/uiGradients), MIT", license: include_str!("../assets/gradients/uiGradients-LICENSE.txt") },
    Entry { name: "WebGradients", what: "Gradient presets (github.com/itmeo/webgradients), MIT", license: include_str!("../assets/gradients/WebGradients-LICENSE.txt") },
    Entry { name: "Noto Color Emoji", what: "Emoji font, SIL Open Font License 1.1", license: include_str!("../assets/fonts/NotoColorEmoji-LICENSE.txt") },
    Entry { name: "Poppins", what: "Text font, SIL Open Font License 1.1", license: include_str!("../assets/fonts/OFL.txt") },
    Entry { name: "Lucide", what: "Icons, ISC License", license: include_str!("../assets/icons/editor/LICENSE-LUCIDE") },
];

struct Credits;

impl Render for Credits {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap(px(ui::S6)).px(px(ui::S6)).pt(px(52.)).pb(px(ui::S6));
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(gpui_kit::img(crate::mascot::image(120)).h(px(52.)).w(px(52. * crate::mascot::aspect())))
                .child(div().flex().flex_col().child(div().text_xl().font_weight(FontWeight::BOLD).child("Credits")).child(div().text_xs().text_color(rgb(MUTED)).child("Clipper is made with these open-source works."))),
        );
        for e in &ENTRIES {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(ui::S2))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(e.name))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(e.what))
                    .child(
                        div()
                            .p(px(ui::S3))
                            .rounded(px(10.))
                            .bg(rgb(crate::CARD))
                            .border_1()
                            .border_color(ui::hairline())
                            .text_xs()
                            .line_height(px(17.))
                            .text_color(rgb(0xc7c7cc))
                            .child(e.license.trim().to_string()),
                    ),
            );
        }
        div().id("credits").size_full().bg(rgb(BG)).text_color(rgb(TEXT)).overflow_y_scrollbar().child(col)
    }
}

pub fn open(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(620.), px(720.)), cx);
    let _ = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Credits".into()), appears_transparent: true, traffic_light_position: Some(point(px(18.), px(19.))) }),
            ..Default::default()
        },
        cx,
        |_, cx| cx.new(|_| Credits),
    );
}
