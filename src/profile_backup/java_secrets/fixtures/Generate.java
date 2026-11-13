import de.kortty.security.EncryptionService;
import java.io.StringWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Base64;
import java.util.Properties;

// Compile with EncryptionService and PerfTrace from Java KorTTY v2.26.0 and slf4j-api.
// Run with the fixture directory as its sole argument. All inputs are public test values.
public class Generate {
    public static void main(String[] args) throws Exception {
        var fixture = Path.of(args[0]);
        var password = "päss🔑例".toCharArray();
        var encryption = new EncryptionService();
        var salt = encryption.generateSalt();
        var properties = new Properties();
        properties.setProperty("salt", Base64.getEncoder().encodeToString(salt));
        properties.setProperty("hash", encryption.hashPassword(password, salt));
        var text = new StringWriter();
        properties.store(text, "KorTTY Master Password");
        Files.writeString(fixture.resolve("master.properties"), text.toString());

        // RFC 8032 test vector 1 seed in a PKCS#8 PEM envelope.
        var key = """
                -----BEGIN PRIVATE KEY-----
                MC4CAQAwBQYDK2VwBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn9g
                -----END PRIVATE KEY-----
                """;
        var lines = String.join("\n",
                encryption.encryptPassword("café – 新🔒", password),
                encryption.encryptPassword("", password),
                "enc:" + encryption.encrypt(key, encryption.deriveKey(password, salt)));
        Files.writeString(fixture.resolve("envelopes.txt"), lines + "\n");
    }
}
