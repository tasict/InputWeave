//! The outputs a Wayland connection follows: each `wl_output` with its
//! `xdg_output`, and what they said of themselves, as the displays
//! (`displays`) and input without portals (`wlr`) both need them. A
//! connection's state hands the outputs' events to its `Outputs` with
//! `wayland_client::delegate_dispatch!`, and the registry's to `add` and
//! `remove`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use iw_core::layout::Display;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_callback, wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

use super::translate::{Output, displays_of};

/// The newest versions of the protocols this knows.
pub(crate) const OUTPUT_VERSION: u32 = 4;
pub(crate) const XDG_OUTPUT_VERSION: u32 = 3;

/// A connection's state that follows outputs.
pub(crate) trait Follows {
    fn outputs(&mut self) -> &mut Outputs;
}

/// An output: its object, and what it said of itself.
pub(crate) struct Out {
    pub output: wl_output::WlOutput,
    xdg: Option<zxdg_output_v1::ZxdgOutputV1>,
    pub described: Output,
}

/// The outputs, and how many times they changed.
pub(crate) struct Outputs {
    pub list: Vec<Out>,
    manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    /// Counts the changes: an output came or went, or finished describing
    /// itself.
    pub changes: u64,
}

impl Outputs {
    /// Following outputs with `manager`'s `xdg_output`s, if there is one.
    pub(crate) fn new(manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>) -> Self {
        Self {
            list: Vec::new(),
            manager,
            changes: 0,
        }
    }

    /// The outputs `globals` lists now, each with its `xdg_output`: the
    /// manager is bound first, so each gets one at once.
    pub(crate) fn listed<S>(globals: &GlobalList, qh: &QueueHandle<S>) -> Self
    where
        S: Dispatch<wl_output::WlOutput, u32>
            + Dispatch<zxdg_output_v1::ZxdgOutputV1, u32>
            + Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()>
            + 'static,
    {
        let mut outputs = Self::new(globals.bind(qh, 1..=XDG_OUTPUT_VERSION, ()).ok());
        globals.contents().with_list(|list| {
            for global in list {
                if global.interface == wl_output::WlOutput::interface().name {
                    outputs.add(globals.registry(), global.name, global.version, qh);
                }
            }
        });
        outputs
    }

    /// The output global `name` came: bound, with its `xdg_output`.
    pub(crate) fn add<S>(
        &mut self,
        registry: &wl_registry::WlRegistry,
        name: u32,
        version: u32,
        qh: &QueueHandle<S>,
    ) where
        S: Dispatch<wl_output::WlOutput, u32>
            + Dispatch<zxdg_output_v1::ZxdgOutputV1, u32>
            + 'static,
    {
        let output: wl_output::WlOutput =
            registry.bind(name, version.min(OUTPUT_VERSION), qh, name);
        let xdg = self
            .manager
            .as_ref()
            .map(|m| m.get_xdg_output(&output, qh, name));
        self.list.push(Out {
            output,
            xdg,
            described: Output {
                global: name,
                scale: 1,
                ..Default::default()
            },
        });
        self.changes += 1;
    }

    /// The global `name` went: if it was an output, it is let go of.
    pub(crate) fn remove(&mut self, name: u32) {
        if let Some(at) = self.list.iter().position(|o| o.described.global == name) {
            let out = self.list.remove(at);
            if let Some(xdg) = out.xdg {
                xdg.destroy();
            }
            if out.output.version() >= 3 {
                out.output.release();
            }
            self.changes += 1;
        }
    }

    /// The displays the outputs described so far make.
    pub(crate) fn displays(&self) -> Vec<Display> {
        let outputs: Vec<Output> = self.list.iter().map(|o| o.described.clone()).collect();
        displays_of(&outputs)
    }

    fn described(&mut self, global: u32) -> Option<&mut Output> {
        self.list
            .iter_mut()
            .find(|o| o.described.global == global)
            .map(|o| &mut o.described)
    }
}

/// Waits until `deadline` for the outputs just listed to describe
/// themselves, then their `xdg_output`s: two roundtrips, with whatever else
/// the connection asked for answered along.
pub(crate) fn describe<S>(
    conn: &Connection,
    queue: &mut EventQueue<S>,
    state: &mut S,
    deadline: Instant,
) -> Result<(), String>
where
    S: Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> + 'static,
{
    for _ in 0..2 {
        super::displays::roundtrip_until(conn, queue, state, deadline)?;
    }
    Ok(())
}

impl<S> Dispatch<wl_output::WlOutput, u32, S> for Outputs
where
    S: Dispatch<wl_output::WlOutput, u32> + Follows,
{
    fn event(
        state: &mut S,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<S>,
    ) {
        let outputs = state.outputs();
        let Some(out) = outputs.described(*global) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                out.position = (x, y);
                out.rotated = matches!(
                    transform,
                    WEnum::Value(
                        wl_output::Transform::_90
                            | wl_output::Transform::_270
                            | wl_output::Transform::Flipped90
                            | wl_output::Transform::Flipped270
                    )
                );
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                if let WEnum::Value(flags) = flags
                    && flags.contains(wl_output::Mode::Current)
                {
                    out.mode = Some((width, height));
                }
            }
            wl_output::Event::Scale { factor } => out.scale = factor,
            wl_output::Event::Name { name } => out.name = Some(name),
            wl_output::Event::Done => outputs.changes += 1,
            _ => {}
        }
    }
}

impl<S> Dispatch<zxdg_output_v1::ZxdgOutputV1, u32, S> for Outputs
where
    S: Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> + Follows,
{
    fn event(
        state: &mut S,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<S>,
    ) {
        let outputs = state.outputs();
        let Some(out) = outputs.described(*global) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => out.logical_position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                out.logical_size = Some((width, height));
            }
            // wl_output's own name (version 4) wins: it is the same, and
            // there whether or not there is an xdg_output.
            zxdg_output_v1::Event::Name { name } => {
                if out.name.is_none() {
                    out.name = Some(name);
                }
            }
            // Before version 3, xdg_output's own done ends its description.
            zxdg_output_v1::Event::Done => outputs.changes += 1,
            _ => {}
        }
    }
}

impl<S> Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, (), S> for Outputs
where
    S: Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()>,
{
    fn event(
        _: &mut S,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<S>,
    ) {
    }
}
