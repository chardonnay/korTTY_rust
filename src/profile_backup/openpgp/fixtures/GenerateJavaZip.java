import java.nio.file.Files;
import java.nio.file.Path;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

// Reproduce the plain ZIP layout of Java kortty v2.26.0 BackupManager.
public class GenerateJavaZip {
    public static void main(String[] args) throws Exception {
        try (var output = new ZipOutputStream(Files.newOutputStream(Path.of(args[0])))) {
            add(output, "connections.xml", """
                <connections><connection id="pgp-fixture"><name>GnuPG SSH</name>
                <host>openpgp.example</host><username>fixture</username></connection></connections>
                """);
            add(output, "projects/GnuPG Project.kortty", """
                <project><name>GnuPG Project</name><windows><window><tabs>
                <tab><connectionId>pgp-fixture</connectionId></tab>
                </tabs></window></windows></project>
                """);
            add(output, "credentials.xml", "<credentials/>");
            add(output, "memory/empty.md", "");
        }
    }

    private static void add(ZipOutputStream output, String name, String text) throws Exception {
        var member = new ZipEntry(name);
        member.setTime(1_700_000_000_000L);
        output.putNextEntry(member);
        output.write(text.getBytes(java.nio.charset.StandardCharsets.UTF_8));
        output.closeEntry();
    }
}
