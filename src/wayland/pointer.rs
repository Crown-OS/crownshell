use smithay_client_toolkit::{
    delegate_pointer,
    seat::pointer::{AxisScroll, PointerEvent, PointerEventKind, PointerHandler},
    shell::WaylandSurface,
};
use wayland_client::{Connection, QueueHandle, protocol::wl_pointer};

use crate::{
    app::App,
    handler::{PointerButton, ScrollDelta},
};

/// Folds a frame's two axes into one delta, keeping the continuous and the
/// discrete measurement side by side — a wheel fills in `discrete`, a touchpad
/// only `absolute`, and a handler cannot recover one from the other.
fn scroll_delta(horizontal: &AxisScroll, vertical: &AxisScroll) -> ScrollDelta {
    ScrollDelta {
        x: horizontal.absolute,
        y: vertical.absolute,
        discrete_x: horizontal.discrete,
        discrete_y: vertical.discrete,
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let App {
            compositor_state,
            qh,
            text_cx,
            windows,
            ..
        } = self;
        for event in events {
            let Some(window) = windows
                .iter_mut()
                .find(|w| w.layer.wl_surface() == &event.surface)
            else {
                continue;
            };
            let (x, y) = event.position;
            match &event.kind {
                PointerEventKind::Enter { .. } => {
                    window.on_pointer_enter(x, y, compositor_state, qh, text_cx)
                }
                PointerEventKind::Leave { .. } => {
                    window.on_pointer_leave(compositor_state, qh, text_cx)
                }
                PointerEventKind::Motion { .. } => {
                    window.on_pointer_motion(x, y, compositor_state, qh, text_cx)
                }
                PointerEventKind::Press { button, .. } => window.on_pointer_button(
                    x,
                    y,
                    PointerButton::from_code(*button),
                    true,
                    compositor_state,
                    qh,
                    text_cx,
                ),
                PointerEventKind::Release { button, .. } => window.on_pointer_button(
                    x,
                    y,
                    PointerButton::from_code(*button),
                    false,
                    compositor_state,
                    qh,
                    text_cx,
                ),
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => window.on_pointer_scroll(
                    x,
                    y,
                    scroll_delta(horizontal, vertical),
                    compositor_state,
                    qh,
                    text_cx,
                ),
            }
        }
    }
}

delegate_pointer!(App);

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(absolute: f64, discrete: i32) -> AxisScroll {
        AxisScroll {
            absolute,
            discrete,
            stop: false,
        }
    }

    #[test]
    fn wheel_notches_survive_as_discrete_steps() {
        let delta = scroll_delta(&AxisScroll::default(), &axis(15.0, 1));
        assert_eq!(
            delta,
            ScrollDelta {
                x: 0.0,
                y: 15.0,
                discrete_x: 0,
                discrete_y: 1,
            }
        );
    }

    #[test]
    fn touchpad_scrolling_keeps_its_continuous_delta_and_no_steps() {
        let delta = scroll_delta(&axis(-3.5, 0), &axis(7.25, 0));
        assert_eq!(delta.x, -3.5);
        assert_eq!(delta.y, 7.25);
        assert_eq!(delta.discrete_x, 0);
        assert_eq!(delta.discrete_y, 0);
    }

    #[test]
    fn an_axis_that_only_stopped_reads_as_no_movement() {
        let stop = AxisScroll {
            absolute: 0.0,
            discrete: 0,
            stop: true,
        };
        assert_eq!(scroll_delta(&stop, &stop), ScrollDelta::default());
    }
}
