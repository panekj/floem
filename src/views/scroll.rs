use floem_reactive::create_effect;
use floem_renderer::Renderer;
use kurbo::{Point, Rect, Size, Vec2};
use peniko::Color;
use taffy::{prelude::Node, style::Position};

use crate::{
    context::{AppState, LayoutCx, PaintCx},
    event::Event,
    id::Id,
    style::{ComputedStyle, Style, StyleValue},
    unit::PxPct,
    view::{ChangeFlags, View},
};

enum ScrollState {
    EnsureVisible(Rect),
    ScrollDelta(Vec2),
    ScrollTo(Point),
    HiddenBar(bool),
    PropagatePointerWheel(bool),
    VerticalScrollAsHorizontal(bool),
}

/// Minimum length for any scrollbar to be when measured on that
/// scrollbar's primary axis.
const SCROLLBAR_MIN_SIZE: f64 = 10.0;

/// Denotes which scrollbar, if any, is currently being dragged.
#[derive(Debug, Copy, Clone)]
enum BarHeldState {
    /// Neither scrollbar is being dragged.
    None,
    /// Vertical scrollbar is being dragged. Contains an `f64` with
    /// the initial y-offset of the dragging input.
    Vertical(f64, Vec2),
    /// Horizontal scrollbar is being dragged. Contains an `f64` with
    /// the initial x-offset of the dragging input.
    Horizontal(f64, Vec2),
}

pub struct ScrollBarStyle {
    color: Color,
    rounded: bool,
    hide: bool,
    thickness: f32,
    edge_width: f32,
}
impl ScrollBarStyle {
    pub const BASE: Self = ScrollBarStyle {
        // 179 is 70% of 255 so a 70% alpha factor is the default
        color: Color::rgba8(0, 0, 0, 179),
        rounded: cfg!(target_os = "macos"),
        thickness: 10.,
        edge_width: 0.,
        hide: false,
    };

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
    pub fn rounded(mut self, rounded: bool) -> Self {
        self.rounded = rounded;
        self
    }
    pub fn thickness(mut self, thickness: f32) -> Self {
        self.thickness = thickness;
        self
    }
    pub fn edge_width(mut self, edge_width: f32) -> Self {
        self.edge_width = edge_width;
        self
    }
}

pub struct Scroll<V: View> {
    id: Id,
    child: V,
    // the total of the scroll view, including padding
    size: Size,
    // the actual rect of the scroll view excluding padding
    actual_rect: Rect,
    child_size: Size,
    child_viewport: Rect,
    onscroll: Option<Box<dyn Fn(Rect)>>,
    held: BarHeldState,
    virtual_node: Option<Node>,
    propagate_pointer_wheel: bool,
    vertical_scroll_as_horizontal: bool,
    scroll_bar_style: ScrollBarStyle,
}

pub fn scroll<V: View>(child: V) -> Scroll<V> {
    Scroll {
        id: Id::next(),
        child,
        size: Size::ZERO,
        actual_rect: Rect::ZERO,
        child_size: Size::ZERO,
        child_viewport: Rect::ZERO,
        onscroll: None,
        held: BarHeldState::None,
        virtual_node: None,
        propagate_pointer_wheel: false,
        vertical_scroll_as_horizontal: false,
        scroll_bar_style: ScrollBarStyle::BASE,
    }
}

impl<V: View> Scroll<V> {
    pub fn on_scroll(mut self, onscroll: impl Fn(Rect) + 'static) -> Self {
        self.onscroll = Some(Box::new(onscroll));
        self
    }

    pub fn on_ensure_visible(self, to: impl Fn() -> Rect + 'static) -> Self {
        let id = self.id;
        create_effect(move |_| {
            let rect = to();
            id.update_state(ScrollState::EnsureVisible(rect), true);
        });

        self
    }

    pub fn on_scroll_delta(self, delta: impl Fn() -> Vec2 + 'static) -> Self {
        let id = self.id;
        create_effect(move |_| {
            let delta = delta();
            id.update_state(ScrollState::ScrollDelta(delta), false);
        });

        self
    }

    pub fn on_scroll_to(self, origin: impl Fn() -> Option<Point> + 'static) -> Self {
        let id = self.id;
        create_effect(move |_| {
            if let Some(origin) = origin() {
                id.update_state(ScrollState::ScrollTo(origin), true);
            }
        });

        self
    }

    pub fn hide_bar(self, hide: impl Fn() -> bool + 'static) -> Self {
        let id = self.id;
        create_effect(move |_| {
            id.update_state(ScrollState::HiddenBar(hide()), false);
        });
        self
    }

    pub fn propagate_pointer_wheel(self, value: impl Fn() -> bool + 'static) -> Self {
        let id = self.id;
        create_effect(move |_| {
            id.update_state(ScrollState::PropagatePointerWheel(value()), false);
        });
        self
    }

    pub fn vertical_scroll_as_horizontal(self, value: impl Fn() -> bool + 'static) -> Self {
        let id = self.id;
        create_effect(move |_| {
            id.update_state(ScrollState::VerticalScrollAsHorizontal(value()), false);
        });
        self
    }

    fn scroll_delta(&mut self, app_state: &mut AppState, delta: Vec2) {
        let new_origin = self.child_viewport.origin() + delta;
        self.clamp_child_viewport(app_state, self.child_viewport.with_origin(new_origin));
    }

    fn scroll_to(&mut self, app_state: &mut AppState, origin: Point) {
        self.clamp_child_viewport(app_state, self.child_viewport.with_origin(origin));
    }

    /// Pan the smallest distance that makes the target [`Rect`] visible.
    ///
    /// If the target rect is larger than viewport size, we will prioritize
    /// the region of the target closest to its origin.
    pub fn pan_to_visible(&mut self, app_state: &mut AppState, rect: Rect) {
        /// Given a position and the min and max edges of an axis,
        /// return a delta by which to adjust that axis such that the value
        /// falls between its edges.
        ///
        /// if the value already falls between the two edges, return 0.0.
        fn closest_on_axis(val: f64, min: f64, max: f64) -> f64 {
            assert!(min <= max);
            if val > min && val < max {
                0.0
            } else if val <= min {
                val - min
            } else {
                val - max
            }
        }

        // clamp the target region size to our own size.
        // this means we will show the portion of the target region that
        // includes the origin.
        let target_size = Size::new(
            rect.width().min(self.child_viewport.width()),
            rect.height().min(self.child_viewport.height()),
        );
        let rect = rect.with_size(target_size);

        let x0 = closest_on_axis(
            rect.min_x(),
            self.child_viewport.min_x(),
            self.child_viewport.max_x(),
        );
        let x1 = closest_on_axis(
            rect.max_x(),
            self.child_viewport.min_x(),
            self.child_viewport.max_x(),
        );
        let y0 = closest_on_axis(
            rect.min_y(),
            self.child_viewport.min_y(),
            self.child_viewport.max_y(),
        );
        let y1 = closest_on_axis(
            rect.max_y(),
            self.child_viewport.min_y(),
            self.child_viewport.max_y(),
        );

        let delta_x = if x0.abs() > x1.abs() { x0 } else { x1 };
        let delta_y = if y0.abs() > y1.abs() { y0 } else { y1 };
        let new_origin = self.child_viewport.origin() + Vec2::new(delta_x, delta_y);
        self.clamp_child_viewport(app_state, self.child_viewport.with_origin(new_origin));
    }

    fn update_size(&mut self, app_state: &mut AppState) {
        let child_size = self.child_size;
        let new_child_size = self.child_size(app_state).unwrap_or_default();
        self.child_size = new_child_size;

        let layout = app_state.get_layout(self.id).unwrap();
        self.size = Size::new(layout.size.width as f64, layout.size.height as f64);

        let style = app_state.get_computed_style(self.id);
        let padding_left = match style.padding_left {
            PxPct::Px(padding) => padding as f32,
            PxPct::Pct(pct) => pct as f32 * layout.size.width,
        };
        let padding_right = match style.padding_right {
            PxPct::Px(padding) => padding as f32,
            PxPct::Pct(pct) => pct as f32 * layout.size.width,
        };
        let padding_top = match style.padding_top {
            PxPct::Px(padding) => padding as f32,
            PxPct::Pct(pct) => pct as f32 * layout.size.width,
        };
        let padding_bottom = match style.padding_bottom {
            PxPct::Px(padding) => padding as f32,
            PxPct::Pct(pct) => pct as f32 * layout.size.width,
        };
        let mut actual_rect = self.size.to_rect();
        actual_rect.x0 += padding_left as f64;
        actual_rect.x1 -= padding_right as f64;
        actual_rect.y0 += padding_top as f64;
        actual_rect.y1 -= padding_bottom as f64;
        self.actual_rect = actual_rect;

        if child_size != new_child_size {
            app_state.request_layout(self.id);
        }
    }

    fn clamp_child_viewport(
        &mut self,
        app_state: &mut AppState,
        child_viewport: Rect,
    ) -> Option<()> {
        let actual_rect = self.actual_rect;
        let actual_size = actual_rect.size();
        let width = actual_rect.width();
        let height = actual_rect.height();
        let child_size = self.child_size;

        let mut child_viewport = child_viewport;
        if width >= child_size.width {
            child_viewport.x0 = 0.0;
        } else if child_viewport.x0 > child_size.width - width {
            child_viewport.x0 = child_size.width - width;
        } else if child_viewport.x0 < 0.0 {
            child_viewport.x0 = 0.0;
        }

        if height >= child_size.height {
            child_viewport.y0 = 0.0;
        } else if child_viewport.y0 > child_size.height - height {
            child_viewport.y0 = child_size.height - height;
        } else if child_viewport.y0 < 0.0 {
            child_viewport.y0 = 0.0;
        }
        child_viewport = child_viewport.with_size(actual_size);

        if child_viewport != self.child_viewport {
            app_state.set_viewport(self.child.id(), child_viewport);
            app_state.request_layout(self.id);
            self.child_viewport = child_viewport;
            if let Some(onscroll) = &self.onscroll {
                onscroll(child_viewport);
            }
        }
        Some(())
    }

    fn child_size(&self, app_state: &mut AppState) -> Option<Size> {
        app_state
            .view_states
            .get(&self.id)
            .map(|view| &view.children_nodes)
            .and_then(|nodes| nodes.get(1))
            .and_then(|node| app_state.taffy.layout(*node).ok())
            .map(|layout| Size::new(layout.size.width as f64, layout.size.height as f64))
    }

    fn draw_bars(&self, cx: &mut PaintCx) {
        let edge_width = self.scroll_bar_style.edge_width as f64;
        let scroll_offset = self.child_viewport.origin().to_vec2();
        let radius = |rect: Rect, vertical| {
            if self.scroll_bar_style.rounded {
                if vertical {
                    (rect.x1 - rect.x0) / 2.
                } else {
                    (rect.y1 - rect.y0) / 2.
                }
            } else {
                0.
            }
        };

        let color = self.scroll_bar_style.color;
        if let Some(bounds) = self.calc_vertical_bar_bounds(cx.app_state) {
            let rect = (bounds - scroll_offset).inset(-edge_width / 2.0);
            let rect = rect.to_rounded_rect(radius(rect, true));
            cx.fill(&rect, color, 0.0);
            if edge_width > 0.0 {
                cx.stroke(&rect, color, edge_width);
            }
        }

        // Horizontal bar
        if let Some(bounds) = self.calc_horizontal_bar_bounds(cx.app_state) {
            let rect = (bounds - scroll_offset).inset(-edge_width / 2.0);
            let rect = rect.to_rounded_rect(radius(rect, false));
            cx.fill(&rect, color, 0.0);
            if edge_width > 0.0 {
                cx.stroke(&rect, color, edge_width);
            }
        }
    }

    fn calc_vertical_bar_bounds(&self, _app_state: &mut AppState) -> Option<Rect> {
        let viewport_size = self.child_viewport.size();
        let content_size = self.child_size;
        let scroll_offset = self.child_viewport.origin().to_vec2();

        if viewport_size.height >= content_size.height {
            return None;
        }

        let bar_width = self.scroll_bar_style.thickness as f64;
        let bar_pad = 0.0;

        let percent_visible = viewport_size.height / content_size.height;
        let percent_scrolled = scroll_offset.y / (content_size.height - viewport_size.height);

        let length = (percent_visible * viewport_size.height).ceil();
        // Vertical scroll bar must have ast least the same height as it's width
        let length = length.max(self.scroll_bar_style.thickness as f64);

        let top_y_offset = ((viewport_size.height - length) * percent_scrolled).ceil();
        let bottom_y_offset = top_y_offset + length;

        let x0 = scroll_offset.x + viewport_size.width - bar_width - bar_pad;
        let y0 = scroll_offset.y + top_y_offset;

        let x1 = scroll_offset.x + viewport_size.width - bar_pad;
        let y1 = scroll_offset.y + bottom_y_offset;

        Some(Rect::new(x0, y0, x1, y1))
    }

    fn calc_horizontal_bar_bounds(&self, _app_state: &mut AppState) -> Option<Rect> {
        let viewport_size = self.child_viewport.size();
        let content_size = self.child_size;
        let scroll_offset = self.child_viewport.origin().to_vec2();

        if viewport_size.width >= content_size.width {
            return None;
        }

        let bar_width = self.scroll_bar_style.thickness as f64;
        let bar_pad = 0.0;

        let percent_visible = viewport_size.width / content_size.width;
        let percent_scrolled = scroll_offset.x / (content_size.width - viewport_size.width);

        let length = (percent_visible * viewport_size.width).ceil();
        let length = length.max(SCROLLBAR_MIN_SIZE);

        let horizontal_padding = if viewport_size.height >= content_size.height {
            0.0
        } else {
            bar_pad + bar_pad + bar_width
        };

        let left_x_offset =
            ((viewport_size.width - length - horizontal_padding) * percent_scrolled).ceil();
        let right_x_offset = left_x_offset + length;

        let x0 = scroll_offset.x + left_x_offset;
        let y0 = scroll_offset.y + viewport_size.height - bar_width - bar_pad;

        let x1 = scroll_offset.x + right_x_offset;
        let y1 = scroll_offset.y + viewport_size.height - bar_pad;

        Some(Rect::new(x0, y0, x1, y1))
    }

    fn click_vertical_bar_area(&mut self, app_state: &mut AppState, pos: Point) {
        let new_y = (pos.y / self.actual_rect.height()) * self.child_size.height
            - self.actual_rect.height() / 2.0;
        let mut new_origin = self.child_viewport.origin();
        new_origin.y = new_y;
        self.scroll_to(app_state, new_origin);
    }

    fn click_horizontal_bar_area(&mut self, app_state: &mut AppState, pos: Point) {
        let new_x = (pos.x / self.actual_rect.width()) * self.child_size.width
            - self.actual_rect.width() / 2.0;
        let mut new_origin = self.child_viewport.origin();
        new_origin.x = new_x;
        self.scroll_to(app_state, new_origin);
    }

    fn point_within_vertical_bar(&self, app_state: &mut AppState, pos: Point) -> bool {
        let viewport_size = self.child_viewport.size();
        let scroll_offset = self.child_viewport.origin().to_vec2();

        if let Some(mut bounds) = self.calc_vertical_bar_bounds(app_state) {
            // Stretch hitbox to edge of widget
            bounds.x1 = scroll_offset.x + viewport_size.width;
            pos.x >= bounds.x0 && pos.x <= bounds.x1
        } else {
            false
        }
    }

    fn point_within_horizontal_bar(&self, app_state: &mut AppState, pos: Point) -> bool {
        let viewport_size = self.child_viewport.size();
        let scroll_offset = self.child_viewport.origin().to_vec2();

        if let Some(mut bounds) = self.calc_horizontal_bar_bounds(app_state) {
            // Stretch hitbox to edge of widget
            bounds.y1 = scroll_offset.y + viewport_size.height;
            pos.y >= bounds.y0 && pos.y <= bounds.y1
        } else {
            false
        }
    }

    fn point_hits_vertical_bar(&self, app_state: &mut AppState, pos: Point) -> bool {
        let viewport_size = self.child_viewport.size();
        let scroll_offset = self.child_viewport.origin().to_vec2();

        if let Some(mut bounds) = self.calc_vertical_bar_bounds(app_state) {
            // Stretch hitbox to edge of widget
            bounds.x1 = scroll_offset.x + viewport_size.width;
            bounds.contains(pos)
        } else {
            false
        }
    }

    fn point_hits_horizontal_bar(&self, app_state: &mut AppState, pos: Point) -> bool {
        let viewport_size = self.child_viewport.size();
        let scroll_offset = self.child_viewport.origin().to_vec2();

        if let Some(mut bounds) = self.calc_horizontal_bar_bounds(app_state) {
            // Stretch hitbox to edge of widget
            bounds.y1 = scroll_offset.y + viewport_size.height;
            bounds.contains(pos)
        } else {
            false
        }
    }

    /// true if either scrollbar is currently held down/being dragged
    fn are_bars_held(&self) -> bool {
        !matches!(self.held, BarHeldState::None)
    }
}

impl<V: View> View for Scroll<V> {
    fn id(&self) -> Id {
        self.id
    }

    fn child(&self, id: Id) -> Option<&dyn View> {
        if self.child.id() == id {
            Some(&self.child)
        } else {
            None
        }
    }

    fn child_mut(&mut self, id: Id) -> Option<&mut dyn View> {
        if self.child.id() == id {
            Some(&mut self.child)
        } else {
            None
        }
    }

    fn children(&self) -> Vec<&dyn View> {
        vec![&self.child]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn View> {
        vec![&mut self.child]
    }

    fn debug_name(&self) -> std::borrow::Cow<'static, str> {
        "Scroll".into()
    }

    fn update(
        &mut self,
        cx: &mut crate::context::UpdateCx,
        state: Box<dyn std::any::Any>,
    ) -> crate::view::ChangeFlags {
        if let Ok(state) = state.downcast::<ScrollState>() {
            match *state {
                ScrollState::EnsureVisible(rect) => {
                    self.pan_to_visible(cx.app_state, rect);
                }
                ScrollState::ScrollDelta(delta) => {
                    self.scroll_delta(cx.app_state, delta);
                }
                ScrollState::ScrollTo(origin) => {
                    self.scroll_to(cx.app_state, origin);
                }
                ScrollState::HiddenBar(hide) => {
                    self.scroll_bar_style.hide = hide;
                }
                ScrollState::PropagatePointerWheel(value) => {
                    self.propagate_pointer_wheel = value;
                }
                ScrollState::VerticalScrollAsHorizontal(value) => {
                    self.vertical_scroll_as_horizontal = value;
                }
            }
            cx.request_layout(self.id());
            ChangeFlags::LAYOUT
        } else {
            ChangeFlags::empty()
        }
    }

    fn layout(&mut self, cx: &mut crate::context::LayoutCx) -> taffy::prelude::Node {
        cx.layout_node(self.id, true, |cx| {
            if let Some(color) = cx.scroll_bar_color {
                self.scroll_bar_style.color = color;
            }
            if let Some(rounded) = cx.scroll_bar_rounded {
                self.scroll_bar_style.rounded = rounded;
            }
            if let Some(thickness) = cx.scroll_bar_thickness {
                self.scroll_bar_style.thickness = thickness;
            }
            if let Some(edge_width) = cx.scroll_bar_edge_width {
                self.scroll_bar_style.edge_width = edge_width;
            }

            let child_id = self.child.id();
            let child_view = cx.app_state_mut().view_state(child_id);
            child_view.style.position = StyleValue::Val(Position::Absolute);
            let child_node = self.child.layout_main(cx);

            let virtual_style = Style::BASE
                .width(self.child_size.width)
                .height(self.child_size.height)
                .min_width(0.0)
                .min_height(0.0)
                .compute(&ComputedStyle::default())
                .to_taffy_style();
            if self.virtual_node.is_none() {
                self.virtual_node = Some(
                    cx.app_state_mut()
                        .taffy
                        .new_leaf(virtual_style.clone())
                        .unwrap(),
                );
            }
            let virtual_node = self.virtual_node.unwrap();
            let _ = cx
                .app_state_mut()
                .taffy
                .set_style(virtual_node, virtual_style);

            vec![virtual_node, child_node]
        })
    }

    fn compute_layout(&mut self, cx: &mut LayoutCx) -> Option<Rect> {
        self.update_size(cx.app_state_mut());
        self.clamp_child_viewport(cx.app_state_mut(), self.child_viewport);
        self.child.compute_layout_main(cx);
        None
    }

    fn event(
        &mut self,
        cx: &mut crate::context::EventCx,
        id_path: Option<&[Id]>,
        event: crate::event::Event,
    ) -> bool {
        let viewport_size = self.child_viewport.size();
        let scroll_offset = self.child_viewport.origin().to_vec2();
        let content_size = self.child_size;

        match &event {
            Event::PointerDown(event) => {
                if !self.scroll_bar_style.hide && event.button.is_primary() {
                    self.held = BarHeldState::None;

                    let pos = event.pos + scroll_offset;

                    if self.point_within_vertical_bar(cx.app_state, pos) {
                        if self.point_hits_vertical_bar(cx.app_state, pos) {
                            self.held = BarHeldState::Vertical(
                                // The bounds must be non-empty, because the point hits the scrollbar.
                                event.pos.y,
                                scroll_offset,
                            );
                            cx.update_active(self.id);
                            return true;
                        }
                        self.click_vertical_bar_area(cx.app_state, event.pos);
                        let scroll_offset = self.child_viewport.origin().to_vec2();
                        self.held = BarHeldState::Vertical(
                            // The bounds must be non-empty, because the point hits the scrollbar.
                            event.pos.y,
                            scroll_offset,
                        );
                        cx.update_active(self.id);
                        return true;
                    } else if self.point_within_horizontal_bar(cx.app_state, pos) {
                        if self.point_hits_horizontal_bar(cx.app_state, pos) {
                            self.held = BarHeldState::Horizontal(
                                // The bounds must be non-empty, because the point hits the scrollbar.
                                event.pos.x,
                                scroll_offset,
                            );
                            cx.update_active(self.id);
                            return true;
                        }
                        self.click_horizontal_bar_area(cx.app_state, event.pos);
                        let scroll_offset = self.child_viewport.origin().to_vec2();
                        self.held = BarHeldState::Horizontal(
                            // The bounds must be non-empty, because the point hits the scrollbar.
                            event.pos.x,
                            scroll_offset,
                        );
                        cx.update_active(self.id);
                        return true;
                    }
                }
            }
            Event::PointerUp(_event) => self.held = BarHeldState::None,
            Event::PointerMove(event) => {
                if !self.scroll_bar_style.hide {
                    if self.are_bars_held() {
                        match self.held {
                            BarHeldState::Vertical(offset, initial_scroll_offset) => {
                                let scale_y = viewport_size.height / content_size.height;
                                let y = initial_scroll_offset.y + (event.pos.y - offset) / scale_y;
                                self.clamp_child_viewport(
                                    cx.app_state,
                                    self.child_viewport
                                        .with_origin(Point::new(initial_scroll_offset.x, y)),
                                );
                            }
                            BarHeldState::Horizontal(offset, initial_scroll_offset) => {
                                let scale_x = viewport_size.width / content_size.width;
                                let x = initial_scroll_offset.x + (event.pos.x - offset) / scale_x;
                                self.clamp_child_viewport(
                                    cx.app_state,
                                    self.child_viewport
                                        .with_origin(Point::new(x, initial_scroll_offset.y)),
                                );
                            }
                            BarHeldState::None => {}
                        }
                    } else {
                        let pos = event.pos + scroll_offset;
                        if self.point_within_vertical_bar(cx.app_state, pos)
                            || self.point_within_horizontal_bar(cx.app_state, pos)
                        {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }

        if cx.should_send(self.child.id(), &event)
            && self.child.event_main(cx, id_path, event.clone())
        {
            return true;
        }

        if let Event::PointerWheel(pointer_event) = &event {
            if let Some(listener) = event.listener() {
                if let Some(action) = cx.get_event_listener(self.id, &listener) {
                    if (*action)(&event) {
                        return true;
                    }
                }
            }
            let delta = pointer_event.delta;
            let delta = if self.vertical_scroll_as_horizontal && delta.x == 0.0 && delta.y != 0.0 {
                Vec2::new(delta.y, delta.x)
            } else {
                delta
            };
            self.clamp_child_viewport(cx.app_state, self.child_viewport + delta);
            return !self.propagate_pointer_wheel;
        }

        false
    }

    fn paint(&mut self, cx: &mut crate::context::PaintCx) {
        cx.save();
        if let Some(color) = cx.scroll_bar_color {
            self.scroll_bar_style.color = color;
        }
        if let Some(rounded) = cx.scroll_bar_rounded {
            self.scroll_bar_style.rounded = rounded;
        }
        if let Some(thickness) = cx.scroll_bar_thickness {
            self.scroll_bar_style.thickness = thickness;
        }
        if let Some(edge_width) = cx.scroll_bar_edge_width {
            self.scroll_bar_style.edge_width = edge_width;
        }
        let style = cx.get_computed_style(self.id);
        let radius = style.border_radius.0;
        if radius > 0.0 {
            let rect = self.actual_rect.to_rounded_rect(radius);
            cx.clip(&rect);
        } else {
            cx.clip(&self.actual_rect);
        }
        cx.offset((-self.child_viewport.x0, -self.child_viewport.y0));
        self.child.paint_main(cx);
        cx.restore();

        if !self.scroll_bar_style.hide {
            self.draw_bars(cx);
        }
    }
}
