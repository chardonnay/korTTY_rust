# korTTY patch

This contains the library sources and MIT notice from fancy-regex 0.17.0.
The Java regex translator opts into two additional VM instructions for
extended grapheme clusters and Java's previous-match-relative grapheme
boundary. Segmentation uses the Unicode 16.0 classifications observed
in OpenJDK 25.0.2. The table generator records those classifications
without requiring a Java runtime for matching. To regenerate the table,
run the command in `generate_java_graphemes.java` and save its output as
`src/java_grapheme_data.rs` with LF line endings.
The patch bounds cumulative segmentation work for each matcher invocation.
Internal backreference escapes apply the Java ASCII or Unicode comparison
mode. Unicode comparisons use the simple lowercase of the simple uppercase
code point, from Unicode 16.0, and measure the consumed UTF-8 bytes in the
candidate. The same per-search work limit bounds Unicode comparisons.
The Java translator enables UTF-16 positions during VM execution and
iteration, including for patterns with ordinary predicates and empty
matches. Reported match ranges use complete UTF-8 characters.
The translator chooses code-unit or code-point search advancement and
can also try the same program at one low-surrogate position. Java word
assertions honor scoped word classes, surrogate positions and the
base-character rule for nonspacing marks.
Standalone Java line endings try CRLF before CR. A directly repeated
line ending commits its choice. Deterministic and possessive repeating
groups commit each completed occurrence.
Java canonical-equivalence classes test NFC forms of grapheme prefixes
and return the original input range. The VM applies the Java
prefix retry rules and charges the normal text-work budget.
Fixed-width lookbehind uses the original pattern's Java
offset rules. Variable-width lookbehind in UTF-16 mode evaluates Java's
width limits, tests the nearest candidate first and checks the original
endpoint. A positive lookbehind commits its first successful candidate.
Backreferences compare the original UTF-16 capture,
including captures inside a supplementary character. In UTF-16 mode,
character classes carry their scalar predicate and separate ranges for
low surrogates.
The translator uses the Unicode 16.0 simple uppercase and case comparison
tables to compile literals and supported bracket classes, including ranges
and negation. Literal runs preserve Java's case semantics and quantifier
boundaries.
To regenerate the table, run `python generate_java_case.py UnicodeData.txt`
with the file from <https://www.unicode.org/Public/16.0.0/ucd/>.
The Unicode data license is in `LICENSE-UNICODE`.
Generate Java's script, block and character name tables with
`python generate_java_unicode.py Blocks.txt PropertyValueAliases.txt UnicodeData.txt`.
All inputs come from the same Unicode 16.0 directory. The Rust matcher
uses its own Unicode 16.0 script tables for membership.
The Java option makes `\G` compare against the previous match end and
retains empty matches immediately after a nonempty match. The default
iteration behavior is unchanged.
The default builder and regex dialect retain their upstream behavior.
