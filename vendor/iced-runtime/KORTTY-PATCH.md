# kortty patch for iced_runtime

The source comes from crates.io `iced_runtime` 0.14.0, Iced commit
`3997291f318a8bc06fa522f5579836fb3feb94df` under `runtime`. The published
archive SHA-256 is `d1889b819ce4c06674183242e336c8d49465665441396914dc07cc86f44fa8d4`.

The internal `window::Action::Open` carries an optional Wayland owner ID.
Ordinary `window::open(settings)` still creates an unowned window. The
[iced_winit patch](../iced-winit/KORTTY-PATCH.md) resolves the ID in its
window manager and attaches the toplevel during native creation.
