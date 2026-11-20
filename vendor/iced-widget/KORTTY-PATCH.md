# kortty patch for iced_widget

The source comes from crates.io `iced_widget` 0.14.2, Iced commit
`54abf81d13fec06d4d9ac754b03ce2c3313e8a1f` under `widget`. The published
archive SHA-256 is `b1596afa0d3109c2618e8bc12bae6c11d3064df8f95c42dfce570397dbe957ab`.

`Scrollbar::minimum_scroller_length` sets a per-axis minimum thumb length.
The Session Journal uses it to keep long captures draggable. For scrollbars
that opt in, the thumb uses its available track travel for both positioning
and pointer input. The default scrollbar geometry follows upstream.

`Scrollable::follow_bottom` lets the Agent Log Table keep the latest rows
visible as content grows. It tracks the previous vertical scroll range
per widget and keeps the horizontal offset. Scrolling away pauses
following. A new revision can explicitly return to the bottom.

`Scrollable::translate_cursor_moves` delivers cursor-move events in the
same content coordinates as the cursor passed to children. The Agent Log
Table and its Settings scroll container opt in so a divider can process
the original positions in a batched gesture after scrolling.

`Scrollable::fixed_header` reserves a separate area above the vertical
viewport. The heading shares the body's horizontal offset and remains
interactive. The Log Table uses it for sort buttons and column dividers.
