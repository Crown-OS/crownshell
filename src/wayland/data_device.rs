use std::io::{BufRead, BufReader};

use calloop::PostAction;
use smithay_client_toolkit::{
    data_device_manager::{
        data_device::DataDeviceHandler,
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
        WritePipe,
    },
    delegate_data_device,
    reexports::client::protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
        wl_data_source::WlDataSource, wl_surface::WlSurface,
    },
    shell::WaylandSurface,
};
use wayland_client::{Connection, QueueHandle};

use crate::{
    app::{ActiveDrag, App, PendingRead},
    handler::{DragOffer as HandlerDragOffer, DropPayload, SurfaceCtx},
};

const ACCEPTED_DND_ACTIONS: DndAction = DndAction::Copy;

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        data_device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        let Some(dd) = self.data_devices.iter().find(|d| d.inner() == data_device) else {
            return;
        };
        let Some(offer) = dd.data().drag_offer() else {
            return;
        };
        let Some(window_idx) = self
            .windows
            .iter()
            .position(|w| w.layer.wl_surface() == surface)
        else {
            offer.accept_mime_type(0, None);
            return;
        };

        let mime_types = offer.with_mime_types(|m| m.to_vec());
        let accepted = {
            let App {
                compositor_state,
                qh,
                windows,
                ..
            } = self;
            let window = &mut windows[window_idx];
            let ctx = SurfaceCtx {
                size: (window.width, window.height),
                compositor_state,
                layer: &window.layer,
                bg_effect_surface: window.bg_effect_surface.as_ref(),
                qh,
            };
            let dofr = HandlerDragOffer {
                mime_types: &mime_types,
                x,
                y,
            };
            window.handler.on_drag_enter(dofr, ctx)
        };

        self.dnd.accept_counter += 1;
        let serial = self.dnd.accept_counter;
        if let Some(mime) = accepted.as_ref() {
            offer.accept_mime_type(serial, Some(mime.clone()));
            offer.set_actions(ACCEPTED_DND_ACTIONS, ACCEPTED_DND_ACTIONS);
        } else {
            offer.accept_mime_type(0, None);
        }

        self.dnd.active = Some(ActiveDrag {
            surface: surface.clone(),
            accepted_mime: accepted,
            pos: (x, y),
        });
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(active) = self.dnd.active.take() else {
            return;
        };
        route_drag_leave(self, &active.surface);
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        x: f64,
        y: f64,
    ) {
        let Some(active) = self.dnd.active.as_mut() else {
            return;
        };
        active.pos = (x, y);
        let surface = active.surface.clone();
        route_drag_motion(self, &surface, x, y);
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        data_device: &WlDataDevice,
    ) {
        let Some(dd) = self.data_devices.iter().find(|d| d.inner() == data_device) else {
            return;
        };
        let Some(offer) = dd.data().drag_offer() else {
            return;
        };
        let Some(active) = self.dnd.active.take() else {
            offer.finish();
            offer.destroy();
            return;
        };
        let Some(mime) = active.accepted_mime else {
            offer.finish();
            offer.destroy();
            return;
        };

        let read_pipe = match offer.receive(mime.clone()) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("open DnD pipe: {e}");
                offer.finish();
                offer.destroy();
                return;
            }
        };

        self.dnd.pending_reads.push(PendingRead {
            offer: offer.clone(),
            surface: active.surface,
            mime_type: mime,
            data: Vec::new(),
            pos: active.pos,
            token: None,
        });
        let key = offer.clone();
        let insert = self.loop_handle.insert_source(
            read_pipe,
            move |_, f, app: &mut App| {
                let Some(idx) = app.dnd.pending_reads.iter().position(|p| p.offer == key) else {
                    return PostAction::Continue;
                };
                let file: &mut std::fs::File = unsafe { f.get_mut() };
                let mut reader = BufReader::new(file);
                match reader.fill_buf() {
                    Ok(buf) if buf.is_empty() => {
                        let entry = app.dnd.pending_reads.remove(idx);
                        entry.offer.finish();
                        entry.offer.destroy();
                        deliver_drop(app, entry);
                        PostAction::Remove
                    }
                    Ok(buf) => {
                        let len = buf.len();
                        app.dnd.pending_reads[idx].data.extend_from_slice(buf);
                        reader.consume(len);
                        PostAction::Continue
                    }
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted) => {
                        PostAction::Continue
                    }
                    Err(e) => {
                        log::warn!("DnD read error: {e}");
                        if let Some(entry) = app.dnd.pending_reads.get(idx) {
                            entry.offer.finish();
                            entry.offer.destroy();
                        }
                        app.dnd.pending_reads.remove(idx);
                        PostAction::Remove
                    }
                }
            },
        );
        match insert {
            Ok(token) => {
                if let Some(last) = self.dnd.pending_reads.last_mut() {
                    last.token = Some(token);
                }
            }
            Err(e) => {
                log::warn!("schedule DnD reader: {e}");
                offer.finish();
                offer.destroy();
                self.dnd.pending_reads.pop();
            }
        }
    }
}

impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        offer: &mut DragOffer,
        actions: DndAction,
    ) {
        let preferred = if actions.contains(DndAction::Copy) {
            DndAction::Copy
        } else {
            DndAction::empty()
        };
        offer.set_actions(ACCEPTED_DND_ACTIONS, preferred);
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

fn route_drag_motion(app: &mut App, surface: &WlSurface, x: f64, y: f64) {
    let Some(idx) = app
        .windows
        .iter()
        .position(|w| w.layer.wl_surface() == surface)
    else {
        return;
    };
    let App {
        compositor_state,
        qh,
        windows,
        ..
    } = app;
    let window = &mut windows[idx];
    let ctx = SurfaceCtx {
        size: (window.width, window.height),
        compositor_state,
        layer: &window.layer,
        bg_effect_surface: window.bg_effect_surface.as_ref(),
        qh,
    };
    if window.handler.on_drag_motion(x, y, ctx) {
        window.request_frame(compositor_state, qh);
    }
}

fn route_drag_leave(app: &mut App, surface: &WlSurface) {
    let Some(idx) = app
        .windows
        .iter()
        .position(|w| w.layer.wl_surface() == surface)
    else {
        return;
    };
    let App {
        compositor_state,
        qh,
        windows,
        ..
    } = app;
    let window = &mut windows[idx];
    let ctx = SurfaceCtx {
        size: (window.width, window.height),
        compositor_state,
        layer: &window.layer,
        bg_effect_surface: window.bg_effect_surface.as_ref(),
        qh,
    };
    if window.handler.on_drag_leave(ctx) {
        window.request_frame(compositor_state, qh);
    }
}

fn deliver_drop(app: &mut App, read: PendingRead) {
    let Some(idx) = app
        .windows
        .iter()
        .position(|w| w.layer.wl_surface() == &read.surface)
    else {
        return;
    };
    let App {
        compositor_state,
        qh,
        windows,
        ..
    } = app;
    let window = &mut windows[idx];
    let ctx = SurfaceCtx {
        size: (window.width, window.height),
        compositor_state,
        layer: &window.layer,
        bg_effect_surface: window.bg_effect_surface.as_ref(),
        qh,
    };
    let drop = DropPayload {
        mime_type: read.mime_type,
        data: read.data,
        x: read.pos.0,
        y: read.pos.1,
    };
    if window.handler.on_drop(drop, ctx) {
        window.request_frame(compositor_state, qh);
    }
}

delegate_data_device!(App);
