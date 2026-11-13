# Java profile backup OpenPGP fixtures

`GenerateJavaZip.java` reproduces the plain `ZipOutputStream` member layout from Java kortty v2.26.0, including the deflated entries and their data descriptors. The connection and project are synthetic. The archive also contains an empty memory file and an empty credential catalogue to exercise the omission report.

`java-default.zip.gpg` is the result of passing that ZIP to GnuPG 2.4.9 with the same encryption flags as Java's `SessionJournalExportProtection`. GnuPG avoids compressing the ZIP again and writes a binary literal packet. `java-compressed.zip.gpg` uses the same ZIP inside one raw Deflate OpenPGP packet. The fixture wrapped the literal before encryption and GnuPG encrypted the prebuilt packets using `--no-compress --no-literal`. Both use one RSA-2048 test recipient and an integrity-protected encrypted packet with MDC.

`java-fixture-secret.asc` is the dedicated synthetic recipient's exported private key. The test-only passphrase is `fixture-only Java GnuPG`. No local profile or existing GnuPG key was used. The fixture needs Java and GnuPG only for regeneration.

Java reference tag: `v2.26.0` at `b235d91f20356f8a9b33ad5398550a043cc21ed2`.
