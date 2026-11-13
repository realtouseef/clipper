//! A dropdown in Clipper's style: a dark rounded trigger with the current choice and a
//! chevron, and a floating menu card (rounded, soft shadow) with a green check on the
//! selected item. Replaces the kit's Select for device pickers.
//!
//! `Dropdown` is an entity: create it with `cx.new`, render it with `.child(entity.clone())`,
//! and subscribe to [`DropdownEvent`] for picks.


use crate::theme as tk;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::*;
use gpui_kit::{
    BoxShadow, Bounds, EventEmitter, FontWeight, MouseButton, Pixels, SharedString, Window, anchored, canvas,
    deferred, div, point, px, rgb,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const TRIGGER_H: f32 = 34.0;

pub enum DropdownEvent {
    /// The user picked the item at this index.
    Select(usize),
}

pub struct Dropdown {
    id: SharedString,
    items: Vec<SharedString>,
    selected: Option<usize>,
    placeholder: SharedString,
    disabled: bool,
    open: bool,
    /// When an outside click closed the menu; a click on the trigger right after must not
    /// reopen it.
    closed_at: Option<Instant>,
    /// Trigger bounds from the last paint (the menu matches its width).
    trigger: Rc<Cell<Bounds<Pixels>>>,
}

impl EventEmitter<DropdownEvent> for Dropdown {}

impl Dropdown {
    pub fn new(id: impl Into<SharedString>, placeholder: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            items: Vec::new(),
            selected: None,
            placeholder: placeholder.into(),
            disabled: false,
            open: false,
            closed_at: None,
            trigger: Rc::new(Cell::new(Bounds::default())),
        }
    }

    pub fn set_items(&mut self, items: Vec<SharedString>, selected: Option<usize>, cx: &mut Context<Self>) {
        self.items = items;
        self.selected = selected.filter(|&i| i < self.items.len());
        cx.notify();
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            self.open &= !disabled;
            cx.notify();
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.closed_at = Some(Instant::now());
            cx.notify();
        }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.disabled || self.items.is_empty() {
            return;
        }
        let just_closed = self.closed_at.is_some_and(|t| t.elapsed() < Duration::from_millis(250));
        if self.open {
            self.close(cx);
        } else if !just_closed {
            self.open = true;
            cx.notify();
        }
    }

    fn pick(&mut self, i: usize, cx: &mut Context<Self>) {
        self.selected = Some(i);
        self.open = false;
        cx.emit(DropdownEvent::Select(i));
        cx.notify();
    }

    fn menu(&self, width: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.items.iter().enumerate().map(|(i, label)| {
            let selected = self.selected == Some(i);
            div()
                .id(("item", i))
                .h(px(32.))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .rounded(px(10.))
                .text_sm()
                .text_color(rgb(if selected { tk::text() } else { tk::text_soft() }))
                .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(tk::hover())).text_color(rgb(tk::text())))
                .child(div().min_w_0().truncate().child(label.clone()))
                .when(selected, |d| d.child(Icon::new(IconName::Check).size_4().text_color(rgb(tk::green_ink()))))
                .on_click(cx.listener(move |this, _, _, cx| this.pick(i, cx)))
        });
        div()
            .id("menu")
            .occlude()
            .mt(px(6.))
            .w(width)
            .p(px(4.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .max_h(px(300.))
            .overflow_y_scroll()
            .rounded(px(14.))
            .bg(rgb(tk::card()))
            .border_1()
            .border_color(rgb(tk::pressed()))
            .shadow(vec![BoxShadow {
                color: tk::shadow(0.55),
                offset: point(px(0.), px(12.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
            .children(items)
    }
}

impl Render for Dropdown {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.selected.and_then(|i| self.items.get(i).cloned());
        let has_value = label.is_some();
        let store = self.trigger.clone();
        let (open, disabled) = (self.open, self.disabled);
        let trigger = div()
            .id(self.id.clone())
            .relative()
            .h(px(TRIGGER_H))
            .w_full()
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded(px(12.))
            .bg(rgb(tk::raised()))
            .border_1()
            .border_color(rgb(if open { tk::green_ink() } else { tk::raised() }))
            .text_sm()
            .when(disabled, |d| d.opacity(0.5))
            .when(!disabled, |d| d.cursor_pointer().hover(|s| s.bg(rgb(tk::hover()))))
            .child(canvas(move |b, _, _| store.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(if has_value { tk::text() } else { tk::muted() }))
                    .child(label.unwrap_or_else(|| self.placeholder.clone())),
            )
            .child(
                Icon::new(if open { IconName::ChevronUp } else { IconName::ChevronDown })
                    .size_4()
                    .text_color(rgb(if open { tk::green_ink() } else { tk::muted() })),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)));
        let width = self.trigger.get().size.width.max(px(160.));
        div()
            .w_full()
            .child(trigger)
            .when(open, |d| {
                d.child(deferred(anchored().snap_to_window_with_margin(px(8.)).child(self.menu(width, cx))).with_priority(1))
            })
    }
}
