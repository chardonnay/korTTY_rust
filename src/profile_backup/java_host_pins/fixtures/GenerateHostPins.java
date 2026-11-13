import java.io.StringWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.Base64;
import java.util.Properties;

// Synthetic public keys. Mirrors the field names and Writer overload in
// SshHostKeyTrustManager.writePins in Java kortty v2.26.0.
class GenerateHostPins {
    public static void main(String[] args) throws Exception {
        var first = "AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX";
        var second = "AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh";
        var properties = new Properties();
        properties.setProperty("format", "1");
        properties.setProperty("entry.count", "3");
        put(properties, 0, "2001:0db8:0000:0000:0000:0000:0000:0001", 2222, first);
        put(properties, 1, "example.test", 22, second);
        put(properties, 2, "café.example", 22, first);
        var output = new StringWriter();
        properties.store(output, "korTTY SSH host keys - verify unexpected changes before editing");
        Files.writeString(Path.of(args[0]), output.toString(), StandardCharsets.UTF_8);
    }

    private static void put(Properties properties, int index, String host, int port, String key)
            throws Exception {
        var prefix = "entry." + index + ".";
        var digest = MessageDigest.getInstance("SHA-256").digest(Base64.getDecoder().decode(key));
        properties.setProperty(prefix + "host", host);
        properties.setProperty(prefix + "port", Integer.toString(port));
        properties.setProperty(prefix + "algorithm", "ssh-ed25519");
        properties.setProperty(prefix + "fingerprintSha256", "SHA256:"
                + Base64.getEncoder().withoutPadding().encodeToString(digest));
        properties.setProperty(prefix + "publicKeyLine", "ssh-ed25519 " + key);
        properties.setProperty(prefix + "trustedAt", "2026-11-13T10:20:30.123456789Z");
    }
}
