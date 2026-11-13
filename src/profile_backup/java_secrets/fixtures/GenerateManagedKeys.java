import de.kortty.core.SSHKeyManager;
import de.kortty.model.SSHKey;
import java.nio.file.Files;
import java.nio.file.Path;

// Compile with the pinned Java KorTTY SSHKeyManager and its dependencies.
// Run with a scratch directory containing a synthetic OpenSSH key as fixture-encrypted.
public class GenerateManagedKeys {
    public static void main(String[] args) throws Exception {
        var profile = Path.of(args[0]);
        var keyFile = profile.resolve("fixture-ed25519.pem");
        Files.writeString(keyFile, """
                -----BEGIN PRIVATE KEY-----
                MC4CAQAwBQYDK2VwBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn9g
                -----END PRIVATE KEY-----
                """);
        var manager = new SSHKeyManager(profile);
        var key = new SSHKey("Managed café", keyFile.toString());
        key.setId("f99ec24c-9aec-4978-a57f-ec37c449ca35");
        key.setDescription("Synthetic Java-managed identity");
        key.setCreatedAt(0);
        manager.copyKeyToUserDir(key);
        manager.setPassphrase(key, "café – 新🔒", "päss🔑例".toCharArray());
        // Persist stable foreign Windows paths after exercising Java's real copy function.
        key.setKeyPath("C:\\Users\\fixture\\.ssh\\fixture-ed25519.pem");
        key.setUserDirPath("C:\\Users\\fixture\\.kortty\\ssh-keys\\"
                + key.getId() + "_fixture-ed25519.pem");
        manager.addKey(key);

        var encryptedFile = profile.resolve("fixture-encrypted");
        var encrypted = new SSHKey("Encrypted Java key", encryptedFile.toString());
        encrypted.setId("fe33d0a9-ab84-4351-8d5c-ece77f40432f");
        encrypted.setCreatedAt(0);
        manager.copyKeyToUserDir(encrypted);
        manager.setPassphrase(encrypted, "fixture-only passphrase", "päss🔑例".toCharArray());
        encrypted.setKeyPath("/home/fixture/.ssh/fixture-encrypted");
        encrypted.setUserDirPath("/home/fixture/.kortty/ssh-keys/"
                + encrypted.getId() + "_fixture-encrypted");
        manager.addKey(encrypted);
        manager.save();
    }
}
