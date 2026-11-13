# kortty patch for iced_widget

The source comes from crates.io `iced_widget` 0.14.2, Iced commit
`54abf81d13fec06d4d9ac754b03ce2c3313e8a1f` under `widget`. The published
archive SHA-256 is `b1596afa0d3109c2618e8bc12bae6c11d3064df8f95c42dfce570397dbe957ab`.

`Scrollbar::minimum_scroller_length` sets a per-axis minimum thumb length.
The Session Journal uses it to keep long captures draggable. For scrollbars
that opt in, the thumb uses its available track travel for both positioning
and pointer input. The default scrollbar geometry follows upstream.
