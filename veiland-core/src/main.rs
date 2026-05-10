// SPDX-License-Identifier: GPL-3.0-or-later


use wayland_client:: {
    protocol::wl_registry,
    Connection, Dispatch, EventQueue, QueueHandle,
};

struct AppData;

impl Dispatch<wl_registry::WlRegistry, ()> for AppData {
    fn event(
        _state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            println!("[{}] {} (v{})", name, interface, version);
        }
    }
}

fn main() {
    println!("veiland-core");
    let conn =  Connection::connect_to_env()
        .expect("failed to connect to Wayland display (is WAYLAND_DISPLAY set?)");
    let display = conn.display();

    let mut event_queue: EventQueue<AppData> = conn.new_event_queue();
    let qh = event_queue.handle();

    let _registry = display.get_registry(&qh, ());

    println!("Advertised globals:");

    event_queue.roundtrip(&mut AppData).expect("Registry roundtrip failed");
}
