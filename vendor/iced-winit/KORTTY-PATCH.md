# kortty patch for iced_winit

The source comes from crates.io `iced_winit` 0.14.1, Iced commit
`38237dd294da3256c2ac4d3f8288fc73956c91e0` under `winit`. The published
archive SHA-256 is `7589888c8e951899cc688247a69933bb7a0511f0b4b2e122ac3fcd5dacb37f72`.

For an owner-scoped Wayland dialog, the native open action resolves the
requested owner by Iced window ID and queues a weak Winit window reference.
The event loop upgrades that reference during creation. After Winit has
configured the new toplevel and before Iced creates its renderer surface,
the shell sends `xdg_toplevel.set_parent` with the live owner on the same
Wayland display. The borrowed Wayland connection queues the request without
dispatching or taking ownership of Winit's display. The two native toplevel
handles are used synchronously on the event loop thread. No raw handle is
stored or sent between threads. If the owner has closed, the shell skips
native creation and acknowledges the reserved ID so the application's
late-open handler can remove any remaining placement state. The application
keeps its owner-scoped input gate and closes a created child through its
regular window lifecycle.

The Linux `wayland-client` dependency uses the system backend and dynamic
libwayland loading, as Winit does. `ObjectId::from_ptr` verifies each proxy's
interface. Sending `set_parent` creates no proxies in the foreign backend's
queue. Its drop removes only that temporary queue because the display is
owned by Winit.

Iced's public `window::run` exposes only the raw window and display traits.
The Wayland raw window handle exposes `wl_surface` and the callback can run
after the first renderer buffer. Winit 0.30.13 exposes the required role
through `WindowExtWayland::xdg_toplevel` on its concrete window. Its Wayland
backend ignores `WindowAttributes::with_parent_window` and `set_visible`, so
an initially hidden window cannot defer the first Wayland map. The patched
creation path uses the public Winit extension without a Winit fork.

For a visible resizable window with temporary equal opening limits, the
shell refreshes those limits after Winit negotiates client decorations. If
the initial configure treated the requested inner size as the outer size,
the shell also restores the requested inner size before creating the
renderer surface. This keeps the titlebar out of the saved content height.

Native acceptance requires a Wayland session. Use a private profile, start
the native binary with `WAYLAND_DEBUG=client`, and open Global Settings and
Quick Connect from one of two main windows. In the trace, verify that the
dialog's `xdg_toplevel.set_parent` names that main window and appears before
the first buffer attach on the dialog's `wl_surface`. Check that the other
main window remains usable, cancellation releases the initiating window, and
closing or moving the dialog follows the compositor and the existing saved
size rules. A compositor only recognizes a mapped toplevel as an owner.

To check the opening limits, reopen Settings and AI Manager on a large
output, a small output, and the large output again. Compare the first and
settled content buffers and the saved dimensions after each close. With
client decorations, the trace should show updated equal limits including
the titlebar before the first buffer, followed by the normal resize limits
after placement.
