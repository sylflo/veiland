# SPDX-License-Identifier: GPL-3.0-or-later
#
# Optional D-Bus companion for the veiland Python plugin SDK. A thin, read-only
# wrapper over jeepney's blocking connection: open a bus, hand its socket to the
# pacer's extra_fds, subscribe to a signal so a change wakes the plugin, and call
# a method / read a property. It is the veiland_svg.py move again -- boilerplate
# that appeared in now_playing.py, promoted to a shared file once wifi/ethernet/
# bluetooth wanted the same shape.
#
# This is a SEPARATE, opt-in file, NOT part of veiland_plugin.py. The SDK stays
# the single vendorable stdlib+ctypes file; D-Bus carries a jeepney dep, so an
# author who wants it vendors this second file (like veiland_svg.py) alongside
# the SDK. jeepney is pure-Python, so there is no C/typelib wiring -- unlike the
# SVG companion it needs nothing on GI_TYPELIB_PATH.
#
# Read-only by design. Every method call is best-effort: a locker plugin must
# never crash on a flaky or absent bus, so a D-Bus hiccup returns None (logged),
# never an exception into the render loop (the no-panic-on-input rule applies to
# external I/O too). The caller buckets that None into a quiet fallback state.
#
# The bus socket goes on FramePacer.events(extra_fds=[conn.fileno()]); a matching
# signal wakes the plugin with no polling. See python/examples/wifi.py for the
# worked NetworkManager pattern and now_playing.py for the MPRIS one.

from __future__ import annotations

import sys
from typing import Any

from jeepney import (
    DBusAddress,
    MatchRule,
    Properties,
    message_bus,
    new_method_call,
)
from jeepney.io.blocking import DBusConnection as _JeepneyConn
from jeepney.io.blocking import open_dbus_connection

__all__ = ["DBusError", "DBusConnection", "log"]


class DBusError(Exception):
    """Base for failures in the D-Bus companion. Mostly unused at runtime -- the
    connection degrades to None rather than raising into the render loop -- but
    connect() raises it when the bus cannot be opened at all, so a plugin can
    decide to run in a permanent fallback state instead of exiting."""


def log(tag: str, msg: str) -> None:
    """One-line tagged stderr log, matching the examples' own log(). Used at the
    D-Bus boundary so a persistent bus failure is diagnosable, not a silent
    "always disconnected" (the same reason now_playing.py logs its catches)."""
    print(f"{tag}: {msg}", file=sys.stderr, flush=True)


class DBusConnection:
    """A read-only blocking D-Bus connection with the four things a status widget
    needs: a fileno() for the pacer, drain_signals() to consume wake-ups, one
    signal subscription, and best-effort method/property reads that return None
    on any error instead of raising. Bus is chosen at connect time ("SESSION" for
    MPRIS, "SYSTEM" for NetworkManager/bluez)."""

    def __init__(self, conn: _JeepneyConn, tag: str) -> None:
        # Constructed via connect(); tag is the plugin name for log lines.
        self._conn = conn
        self._tag = tag

    @classmethod
    def connect(cls, bus: str, tag: str = "veiland-dbus") -> DBusConnection:
        """Open a connection on `bus` ("SESSION" or "SYSTEM"). Raises DBusError
        if the bus is unreachable (no session/system bus, permission) so the
        caller can fall back rather than crash -- opening the bus is the one
        failure worth surfacing; every later call degrades to None instead."""
        try:
            conn = open_dbus_connection(bus=bus)
        except Exception as e:  # noqa: BLE001 -- external I/O, never crash the locker
            raise DBusError(f"could not open {bus} bus: {e}") from e
        return cls(conn, tag)

    def fileno(self) -> int:
        """The bus socket fd, for FramePacer.events(extra_fds=[...]). jeepney's
        blocking connection wraps a real socket at .sock."""
        # int() pins the declared return type: jeepney is untyped, so mypy sees
        # .sock.fileno() as Any and would otherwise widen the signature.
        return int(self._conn.sock.fileno())

    def drain_signals(self) -> None:
        """Consume all currently-queued messages without blocking. The pacer
        selected the fd readable, so at least one is waiting; we do NOT parse
        them -- a matching signal's arrival is the whole message ("something
        changed, re-read"). receive(timeout=0) raises when nothing is left, so
        that (and any transient error) is the loop's clean exit."""
        while True:
            try:
                self._conn.receive(timeout=0)
            except Exception:  # noqa: BLE001 -- empty queue / transient == done
                break

    def subscribe(
        self,
        *,
        interface: str,
        member: str,
        path: str | None = None,
        path_namespace: str | None = None,
        sender: str | None = None,
    ) -> None:
        """Add a match rule so `interface`.`member` signals wake the connection.
        Pass `path` for one object, or `path_namespace` to match a whole subtree
        (NetworkManager and bluez emit PropertiesChanged from many object paths,
        so the namespace form is the usual one). Best-effort: a failed AddMatch
        logs and returns -- the widget still works off its TIMEOUT fallback."""
        rule = MatchRule(
            type="signal",
            interface=interface,
            member=member,
            path=path,
            path_namespace=path_namespace,
            sender=sender,
        )
        try:
            self._conn.send_and_get_reply(message_bus.AddMatch(rule))
        except Exception as e:  # noqa: BLE001 -- degrade to timeout-only refresh
            log(self._tag, f"AddMatch({interface}.{member}) failed: {e}")

    def call(
        self,
        path: str,
        interface: str,
        method: str,
        *,
        bus_name: str,
        signature: str = "",
        body: tuple[Any, ...] = (),
    ) -> Any | None:
        """Call `method` on (bus_name, path, interface) and return the reply
        body (a tuple of out-args), or None on any error. A method with one
        out-arg is reply_body[0]; the caller indexes what it expects. Never
        raises -- a vanished service or malformed reply becomes None, which the
        caller buckets into its fallback state."""
        try:
            addr = DBusAddress(path, bus_name=bus_name, interface=interface)
            reply = self._conn.send_and_get_reply(
                new_method_call(addr, method, signature, body)
            )
            return reply.body
        except Exception as e:  # noqa: BLE001 -- untrusted external I/O
            log(self._tag, f"call {interface}.{method} failed: {e}")
            return None

    def get_prop(
        self, path: str, interface: str, name: str, *, bus_name: str
    ) -> Any | None:
        """Read one property via org.freedesktop.DBus.Properties.Get, unwrapping
        the variant. Returns None on any error (property absent, service gone).
        The value's type is D-Bus-defined; the caller knows what it asked for."""
        try:
            addr = DBusAddress(path, bus_name=bus_name, interface=interface)
            reply = self._conn.send_and_get_reply(Properties(addr).get(name))
            # Get returns a single variant out-arg: (signature, value).
            return reply.body[0][1]
        except Exception as e:  # noqa: BLE001 -- untrusted external I/O
            log(self._tag, f"get {interface}.{name} failed: {e}")
            return None

    def get_all_props(
        self, path: str, interface: str, *, bus_name: str
    ) -> dict[str, Any]:
        """Read every property of `interface` via GetAll, unwrapping the a{sv}
        variant tuples. Returns {} on any error -- an empty dict buckets cleanly
        into "nothing to show" without a None-check at every property read."""
        try:
            addr = DBusAddress(path, bus_name=bus_name, interface=interface)
            reply = self._conn.send_and_get_reply(Properties(addr).get_all())
            # a{sv}: {name: (signature, value)} -> unwrap the variant tuples.
            return {k: v[1] for k, v in reply.body[0].items()}
        except Exception as e:  # noqa: BLE001 -- untrusted external I/O
            log(self._tag, f"get_all {interface} failed: {e}")
            return {}

    def get_managed_objects(
        self, path: str = "/", *, bus_name: str
    ) -> dict[str, dict[str, dict[str, Any]]]:
        """Call org.freedesktop.DBus.ObjectManager.GetManagedObjects, unwrapping
        the a{oa{sa{sv}}} nest to {obj_path: {interface: {prop: value}}}. bluez
        uses this to enumerate adapters + devices in one round-trip. Returns {}
        on any error. Variant unwrap is one level deeper than get_all_props."""
        body = self.call(
            path,
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
            bus_name=bus_name,
        )
        if body is None:
            return {}
        try:
            raw = body[0]
            return {
                obj: {
                    iface: {k: v[1] for k, v in props.items()}
                    for iface, props in ifaces.items()
                }
                for obj, ifaces in raw.items()
            }
        except Exception as e:  # noqa: BLE001 -- malformed reply, degrade to {}
            log(self._tag, f"GetManagedObjects unwrap failed: {e}")
            return {}

    def close(self) -> None:
        try:
            self._conn.close()
        except Exception:  # noqa: BLE001 -- best-effort teardown
            pass
