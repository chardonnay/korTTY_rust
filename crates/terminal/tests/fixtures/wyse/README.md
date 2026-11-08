# Wyse Bytefolgen

Die Testdateien sind lokal zusammengestellte Protokollfolgen. Als Referenz
dienen die fünf Klassen unter `sithtermfx-core/src/main/java/com/sithtermfx/core/emulator/wyse`
in SithTermFX v1.2.7 (`8b02de38b7c6396f1af2558b0b00477cfb9a2871`).

- `wy50.bin` prüft Adressierung, Einfügemodus, Cursor-Speicher, Löschen,
  Feldadressierung und die Statuszeile.
- `wy60.bin` prüft erweiterte Attribute, Umbruchmodus, Parameterverbrauch
  und die Gerätekennung.
- `wy160.bin` prüft den Wechsel auf 132 Spalten, ein rohes Adressbyte über
  0x7f, Farbindizes und die Gerätekennung.

Mit `cargo test --locked -p kortty-terminal --test wyse` ausführen.
