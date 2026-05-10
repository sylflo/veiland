// SPDX-License-Identifier: GPL-3.0-or-later

use std::os::fd::AsFd;

use rustix::fs::{ftruncate, memfd_create, MemfdFlags};
use rustix::mm::{mmap, MapFlags, ProtFlags};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{wl_buffer, wl_compositor, wl_output, wl_registry, wl_shm, wl_shm_pool, wl_surface},
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::{self, ExtSessionLockManagerV1},
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

const BG_COLOR: u32 = 0xFF20_2020;

#[derive(Default, PartialEq)]
enum LockStatus {
    #[default]
    Pending,
    Locked,
    Finished,
}

struct LockSurface {
    surface: wl_surface::WlSurface,
    lock_surface: ExtSessionLockSurfaceV1,
    configured: bool,
}

#[derive(Default)]
struct AppData {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    outputs: Vec<wl_output::WlOutput>,
    lock_manager: Option<ExtSessionLockManagerV1>,
    lock: Option<ExtSessionLockV1>,
    lock_surfaces: Vec<LockSurface>,
    status: LockStatus,
}

impl Dispatch<ExtSessionLockManagerV1, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &ExtSessionLockManagerV1,
        _event: ext_session_lock_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<ExtSessionLockV1, ()> for AppData {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.status = LockStatus::Locked,
            ext_session_lock_v1::Event::Finished => state.status = LockStatus::Finished,
            _ => {}
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &wl_output::WlOutput,
        _event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, usize> for AppData {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        idx: &usize,
        _: &Connection,
        qh: &QueueHandle<AppData>,
    ) {
        let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        else {
            return;
        };

        let shm = state.shm.as_ref().expect("wl_shm not bound").clone();
        let entry = &mut state.lock_surfaces[*idx];
        entry.lock_surface.ack_configure(serial);

        let buffer = make_color_buffer(&shm, qh, width, height, BG_COLOR);
        entry.surface.attach(Some(&buffer), 0, 0);
        entry.surface.damage_buffer(0, 0, width as i32, height as i32);
        entry.surface.commit();
        buffer.destroy();

        entry.configured = true;
        println!(" -> lock surface {} configured ({}x{})", idx, width, height);
    }
}

fn make_color_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<AppData>,
    width: u32,
    height: u32,
    color: u32,
) -> wl_buffer::WlBuffer {
    let stride = width * 4;
    let size = (stride * height) as usize;

    let fd = memfd_create("veiland-shm", MemfdFlags::CLOEXEC).expect("memfd_create");
    ftruncate(&fd, size as u64).expect("ftruncate");

    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED,
            &fd,
            0,
        )
        .expect("mmap")
    };

    unsafe {
        let pixels = std::slice::from_raw_parts_mut(ptr as *mut u32, (width * height) as usize);
        pixels.fill(color);
    }

    let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    buffer
}

impl Dispatch<wl_registry::WlRegistry, ()> for AppData {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<AppData>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "ext_session_lock_manager_v1" => {
                    let manager =
                        registry.bind::<ExtSessionLockManagerV1, _, _>(name, version, qh, ());
                    state.lock_manager = Some(manager);
                    println!(" -> bound lock manager");
                }
                "wl_compositor" => {
                    let compositor =
                        registry.bind::<wl_compositor::WlCompositor, _, _>(name, version, qh, ());
                    state.compositor = Some(compositor);
                    println!(" -> bound wl_compositor");
                }
                "wl_output" => {
                    let output = registry.bind::<wl_output::WlOutput, _, _>(name, version, qh, ());
                    state.outputs.push(output);
                    println!(" -> bound wl_output");
                }
                "wl_shm" => {
                    let shm = registry.bind::<wl_shm::WlShm, _, _>(name, version, qh, ());
                    state.shm = Some(shm);
                    println!(" -> bound wl_shm");
                }
                _ => {}
            }
        }
    }
}

fn main() {
    println!("veiland-core");

    let mut state = AppData::default();

    let conn = Connection::connect_to_env()
        .expect("failed to connect to Wayland display (is WAYLAND_DISPLAY set?)");
    let display = conn.display();

    let mut event_queue: EventQueue<AppData> = conn.new_event_queue();
    let qh = event_queue.handle();

    let _registry = display.get_registry(&qh, ());

    event_queue
        .roundtrip(&mut state)
        .expect("Registry roundtrip failed");

    let compositor = state
        .compositor
        .as_ref()
        .expect("wl_compositor not advertised")
        .clone();
    let lock_manager = state
        .lock_manager
        .as_ref()
        .expect("compositor does not implement ext-session-lock-v1")
        .clone();
    assert!(state.shm.is_some(), "wl_shm not advertised");
    assert!(!state.outputs.is_empty(), "no outputs advertised");

    let lock = lock_manager.lock(&qh, ());
    state.lock = Some(lock.clone());

    let outputs = state.outputs.clone();
    for (idx, output) in outputs.iter().enumerate() {
        let surface = compositor.create_surface(&qh, ());
        let lock_surface = lock.get_lock_surface(&surface, output, &qh, idx);
        state.lock_surfaces.push(LockSurface {
            surface,
            lock_surface,
            configured: false,
        });
    }

    while state.status == LockStatus::Pending {
        event_queue
            .blocking_dispatch(&mut state)
            .expect("event dispatch failed during lock wait");
    }

    match state.status {
        LockStatus::Pending => unreachable!(),
        LockStatus::Finished => {
            eprintln!("Compositor refused the lock");
            for ls in state.lock_surfaces.drain(..) {
                ls.lock_surface.destroy();
                ls.surface.destroy();
            }
            if let Some(lock) = state.lock.take() {
                lock.destroy();
                let _ = event_queue.roundtrip(&mut state);
            }
            std::process::exit(1);
        }
        LockStatus::Locked => {
            println!("locked. Sleeping 3s, then unlocking.");
            std::thread::sleep(std::time::Duration::from_secs(3));
            for ls in state.lock_surfaces.drain(..) {
                ls.lock_surface.destroy();
                ls.surface.destroy();
            }
            let lock = state.lock.take().expect("We hold a lock here");
            lock.unlock_and_destroy();
            event_queue.roundtrip(&mut state).expect("Flush unlock");
            println!("unlocked, exiting");
        }
    }
}
