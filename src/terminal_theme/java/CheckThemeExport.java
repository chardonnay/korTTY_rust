import de.kortty.core.ThemeManager;
import java.nio.file.Path;
import java.util.List;

// Executed against classes compiled from the pinned Java application.
class CheckThemeExport {
    public static void main(String[] args) {
        var manager = new ThemeManager(Path.of(args[0]));
        manager.load();
        var theme = manager.getTheme("NORD").orElseThrow();
        if (!theme.getName().equals("Custom & <Nord>")
                || theme.getFontSize() != 21
                || !theme.getForegroundColor().equals("#123456")
                || !theme.getCursorStyle().equals("STEADY_VERTICAL_BAR")
                || !theme.getBackgroundImagePath().equals("C:\\Private\\a & b.png")) {
            throw new AssertionError("Java did not read the exported definition");
        }
        var panel = List.of(
            theme.getAgentPanelBackgroundColor(), theme.getAgentPanelBorderColor(),
            theme.getAgentPanelTextColor(), theme.getAgentPanelMutedTextColor(),
            theme.getAgentPanelAccentColor(), theme.getAgentPanelErrorColor());
        if (!panel.equals(List.of("#203040", "#324252", "#F0E0D0", "#A0B0C0", "#517191", "#E05060"))) {
            throw new AssertionError("Java did not read all six edited Agent colors");
        }
        manager.save();
        System.out.println("Pinned Java ThemeManager read and saved the Rust theme.");
    }
}
