# SPDX-License-Identifier: GPL-3.0-or-later
#
# Pure-logic tests for the veiland_dbus companion. These do NOT touch a real bus
# (CI has none): a FakeConn stands in for jeepney's blocking connection, returning
# canned reply bodies or raising, so the tests exercise only the companion's
# variant-unwrap + degrade-to-None/{} behaviour. Same shape as test_buffer_chain
# .py, which stubs LinearBuffer so no GBM is allocated.
#
# The per-plugin pick_icon() bucketers are deliberately NOT tested here: reaching
# them means importing a status plugin, which imports veiland_svg -> librsvg ->
# GdkPixbuf. That graphics stack is on the dev shell but not the minimal CI
# python env, so importing it would make a "pure logic" test depend on the whole
# SVG toolchain. Keeping this suite dependency-free (jeepney only) is worth more
# than covering three-line if/else chains.

from __future__ import annotations

from typing import Any, cast

import veiland_dbus as vd

# --------------------------------------------------------------------- fakes


class FakeReply:
    """A jeepney reply stand-in: the companion only reads `.body` off it."""

    def __init__(self, body: tuple[Any, ...]) -> None:
        self.body = body


class FakeSock:
    def fileno(self) -> int:
        return 42


class FakeConn:
    """Stands in for jeepney's blocking connection. send_and_get_reply returns a
    queued FakeReply, or raises the queued exception -- letting a test drive both
    the happy unwrap path and the degrade-on-error path without a bus. Records
    nothing it doesn't need to; the companion only calls send_and_get_reply
    (method/property reads + AddMatch) and reads .sock for fileno()."""

    def __init__(self, reply: Any = None, raises: Exception | None = None) -> None:
        self._reply = reply
        self._raises = raises
        self.sent: list[Any] = []
        self.sock = FakeSock()

    def send_and_get_reply(self, msg: Any) -> Any:
        self.sent.append(msg)
        if self._raises is not None:
            raise self._raises
        return self._reply

    def close(self) -> None:
        pass


def conn_with(reply_body: tuple[Any, ...]) -> vd.DBusConnection:
    """A DBusConnection whose transport returns one canned reply body."""
    return vd.DBusConnection(cast(Any, FakeConn(reply=FakeReply(reply_body))), "test")


def conn_raising() -> tuple[vd.DBusConnection, FakeConn]:
    """A DBusConnection whose transport always raises -- for the degrade paths."""
    fake = FakeConn(raises=RuntimeError("bus gone"))
    return vd.DBusConnection(cast(Any, fake), "test"), fake


# --------------------------------------------------- companion: happy unwrap


def test_get_all_props_unwraps_variant_tuples():
    # a{sv} arrives as {name: (signature, value)}; get_all_props returns
    # {name: value}. This is the unwrap MPRIS/NM GetAll reads depend on.
    body = ({"Powered": ("b", True), "Address": ("s", "AA:BB")},)
    props = conn_with(body).get_all_props("/x", "iface.X", bus_name="org.svc")
    assert props == {"Powered": True, "Address": "AA:BB"}


def test_get_prop_unwraps_single_variant():
    # Get returns one variant out-arg (signature, value); get_prop returns value.
    conn = conn_with((("u", 100),))
    val = conn.get_prop("/x", "iface.X", "State", bus_name="org.svc")
    assert val == 100


def test_get_managed_objects_unwraps_nested_dict():
    # a{oa{sa{sv}}} -> {obj_path: {iface: {prop: value}}}. The bluez shape:
    # an adapter object and a device object, each with variant-wrapped props.
    raw = (
        {
            "/org/bluez/hci0": {"org.bluez.Adapter1": {"Powered": ("b", True)}},
            "/org/bluez/hci0/dev_AA": {
                "org.bluez.Device1": {"Connected": ("b", True), "Name": ("s", "Buds")}
            },
        },
    )
    objs = conn_with(raw).get_managed_objects(bus_name="org.bluez")
    assert objs == {
        "/org/bluez/hci0": {"org.bluez.Adapter1": {"Powered": True}},
        "/org/bluez/hci0/dev_AA": {
            "org.bluez.Device1": {"Connected": True, "Name": "Buds"}
        },
    }


def test_call_returns_reply_body():
    # call() returns the whole reply body tuple; the caller indexes out-args.
    body = (["a", "b"],)
    assert conn_with(body).call("/x", "iface.X", "M", bus_name="org.svc") == body


def test_fileno_returns_socket_fileno_as_int():
    c = conn_with(())
    fd = c.fileno()
    assert fd == 42 and isinstance(fd, int)


# ------------------------------------------------ companion: degrade on error


def test_get_all_props_degrades_to_empty_dict():
    conn, _ = conn_raising()
    assert conn.get_all_props("/x", "iface.X", bus_name="org.svc") == {}


def test_get_prop_degrades_to_none():
    conn, _ = conn_raising()
    assert conn.get_prop("/x", "iface.X", "State", bus_name="org.svc") is None


def test_call_degrades_to_none():
    conn, _ = conn_raising()
    assert conn.call("/x", "iface.X", "M", bus_name="org.svc") is None


def test_get_managed_objects_degrades_to_empty_dict():
    conn, _ = conn_raising()
    assert conn.get_managed_objects(bus_name="org.bluez") == {}


def test_get_managed_objects_degrades_on_malformed_body():
    # A reply that is not the expected a{oa{sa{sv}}} nest must not raise out of
    # the widget -- get_managed_objects returns {} rather than propagating.
    objs = conn_with(("not a dict",)).get_managed_objects(bus_name="org.bluez")
    assert objs == {}


def test_subscribe_swallows_addmatch_failure():
    # AddMatch is best-effort: a failure must not raise (the widget still works
    # off its TIMEOUT fallback). It should have attempted the send.
    conn, fake = conn_raising()
    conn.subscribe(
        interface="org.freedesktop.DBus.Properties",
        member="PropertiesChanged",
        path_namespace="/org/bluez",
    )
    assert len(fake.sent) == 1  # tried once, swallowed the error


def test_subscribe_builds_a_matchrule_addmatch():
    # On the happy path subscribe sends exactly one message (the AddMatch).
    fake = FakeConn(reply=FakeReply((None,)))
    conn = vd.DBusConnection(cast(Any, fake), "test")
    conn.subscribe(interface="iface.X", member="Changed", path="/x")
    assert len(fake.sent) == 1
