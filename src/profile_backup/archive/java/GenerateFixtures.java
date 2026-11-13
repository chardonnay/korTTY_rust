import java.nio.file.Files;
import java.nio.file.Path;
import net.lingala.zip4j.ZipFile;
import net.lingala.zip4j.model.ZipParameters;
import net.lingala.zip4j.model.enums.AesKeyStrength;
import net.lingala.zip4j.model.enums.EncryptionMethod;

// Reproduce the ZIP producer calls in Java kortty v2.26.0 BackupManager.
public class GenerateFixtures {
    public static void main(String[] args) throws Exception {
        Path run = Path.of(args[0]);
        Path input = Files.createDirectories(run.resolve("input"));
        Files.writeString(input.resolve("connections.xml"), "<connections/>\n");
        Path projects = Files.createDirectories(input.resolve("projects"));
        Files.writeString(projects.resolve("München.kortty"), "<project/>\n");
        Files.createDirectories(input.resolve("ssh-keys"));
        Files.createDirectories(input.resolve("memory").resolve("nested"));
        Files.write(input.resolve("memory/empty.md"), new byte[0]);

        Path output = Path.of(args[1]);
        create(input, output.resolve("java-aes.zip"), "päss🔑", EncryptionMethod.AES);
        create(input, output.resolve("java-legacy.zip"), "backup-pw", EncryptionMethod.ZIP_STANDARD);
    }

    private static void create(Path input, Path output, String password, EncryptionMethod method) throws Exception {
        ZipParameters parameters = new ZipParameters();
        parameters.setEncryptFiles(true);
        parameters.setEncryptionMethod(method);
        parameters.setAesKeyStrength(AesKeyStrength.KEY_STRENGTH_256);
        try (ZipFile zip = new ZipFile(output.toFile(), password.toCharArray())) {
            ZipParameters fileParameters = new ZipParameters(parameters);
            fileParameters.setFileNameInZip("connections.xml");
            zip.addFile(input.resolve("connections.xml").toFile(), fileParameters);
            for (String name : new String[] {"projects", "ssh-keys", "memory"}) {
                zip.addFolder(input.resolve(name).toFile(), parameters);
            }
        }
    }
}
