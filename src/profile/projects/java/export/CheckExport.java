import de.kortty.core.ProjectManager;
import de.kortty.model.ServerConnection;
import de.kortty.model.SessionState;
import de.kortty.model.SplitPaneState;
import de.kortty.persistence.XMLConnectionRepository;
import java.nio.file.Path;
import java.util.List;
import java.util.UUID;

// Test harness executed against the original Java application, with an isolated profile.
class CheckExport {
    private static final String PRIMARY = "94144051-b1fb-40fb-bec7-a160f119ae9e";
    private static final String ALTERNATE = "bf733aa6-04db-4f8e-b8c5-b8a0d3e41710";
    private static final String SERIAL = "4175c719-1323-448f-adc1-055bc6879e80";

    static void check(boolean actual, String description) {
        if (!actual) {
            throw new AssertionError(description);
        }
    }

    static ServerConnection connection(List<ServerConnection> connections, String id) {
        return connections.stream().filter(connection -> id.equals(connection.getId())).findFirst().orElseThrow();
    }

    static Object invoke(Object receiver, String method) throws Exception {
        var accessor = receiver.getClass().getDeclaredMethod(method);
        accessor.setAccessible(true);
        return accessor.invoke(receiver);
    }

    public static void main(String[] args) throws Exception {
        var directory = Path.of(args[1]);
        var project = new ProjectManager(directory).loadProject(Path.of(args[0]));
        if (args.length == 4 && args[2].equals("remote-tools")) {
            var windows = project.getWindows();
            check(windows.size() == 1, "remote tool window count");
            var window = windows.getFirst();
            var tabs = window.getTabs();
            check(tabs.size() == 3 && window.getActiveTabIndex() == 2
                && window.getActiveSessionId().equals(tabs.get(2).getSessionId()), "remote tab order and selection");
            var manager = tabs.get(0);
            check(manager.getTabType() == SessionState.TabType.SFTP_MANAGER
                && PRIMARY.equals(manager.getConnectionId())
                && args[3].equals(manager.getSftpLocalPath())
                && "/srv/deployment".equals(manager.getSftpRemotePath())
                && Integer.valueOf(15).equals(manager.getSftpAutoCloseTimeout()), "SFTP tab state");
            var editor = tabs.get(1);
            check(editor.getTabType() == SessionState.TabType.FILE_EDITOR
                && ALTERNATE.equals(editor.getConnectionId())
                && Boolean.TRUE.equals(editor.getEditorIsRemote())
                && "/etc/fixture ü.txt".equals(editor.getEditorFilePath()), "independent remote editor route");
            var image = tabs.get(2);
            check(image.getTabType() == SessionState.TabType.IMAGE_VIEWER
                && PRIMARY.equals(image.getConnectionId())
                && Boolean.TRUE.equals(image.getImageIsRemote())
                && "/srv/deployment/topology.png".equals(image.getImageFilePath())
                && Double.valueOf(1.75).equals(image.getImageZoomLevel()), "remote image zoom and route");
            System.out.println("pinned Java loader accepted the Rust remote tool tabs");
            return;
        }
        check(project.getName().equals("Native verified") && project.isAutoReconnect(), "project metadata");
        var windows = project.getWindows();
        check(windows.size() == 2, "window count");

        var first = windows.get(0);
        UUID.fromString(first.getWindowId());
        var geometry = first.getGeometry();
        check(geometry.getWidth() == 910 && geometry.getHeight() == 680
            && geometry.getX() == 35 && geometry.getY() == 61, "first window geometry");
        check(first.getTabs().size() == 3 && first.getActiveTabIndex() == 1, "first window tabs");
        var editor = first.getTabs().get(2);
        check(editor.getTabType() == SessionState.TabType.FILE_EDITOR
            && "/tmp/fixture-editor.txt".equals(editor.getEditorFilePath())
            && Boolean.FALSE.equals(editor.getEditorIsRemote()), "local editor path");
        var local = first.getTabs().get(0);
        check(Boolean.TRUE.equals(local.getLocalShell()) && local.getFontSizeOverride() == null,
            "default local shell inherits the global font");
        check("mother".equals(local.getTerminalEffectPluginId())
            && local.getTerminalEffectAnimationSpeed() == null, "default terminal effect speed");
        UUID.fromString(local.getConnectionId());
        var localResolver = Class.forName("de.kortty.ui.TabRestoreTriage")
            .getDeclaredMethod("unsavedLocalShell", SessionState.class, String.class, String.class);
        localResolver.setAccessible(true);
        var shell = (ServerConnection) localResolver.invoke(null, local, "/bin/sh", "Local Shell");
        check(shell.isLocalShell() && shell.getLocalShellCommand().equals("/bin/sh")
            && shell.getId().equals(local.getConnectionId()), "Java default shell resolution");
        var ssh = first.getTabs().get(1);
        check(first.getActiveSessionId().equals(ssh.getSessionId()), "first active session ID");
        check(ssh.getConnectionId().equals(PRIMARY) && ssh.getFontSizeOverride() == 19, "SSH tab link");
        check("neon-city".equals(ssh.getTerminalEffectPluginId())
            && Double.valueOf(7.0).equals(ssh.getTerminalEffectAnimationSpeed()), "tab-wide terminal effect");

        var split = ssh.getSplitPaneState();
        var nested = split.getRightChild();
        check(split.getOrientation().equals("HORIZONTAL") && split.getDividerPosition() == 0.37, "outer split");
        check(nested.getOrientation().equals("VERTICAL") && nested.getDividerPosition() == 0.68, "inner split");
        check(split.getLeftChild().getWidgetIndex() == 0 && nested.getLeftChild().getWidgetIndex() == 1
            && nested.getRightChild().getWidgetIndex() == 2, "split widget order");

        var planner = Class.forName("de.kortty.ui.SplitLayoutRestorePlan").getDeclaredMethod("plan", SplitPaneState.class);
        planner.setAccessible(true);
        var plan = planner.invoke(null, split);
        var steps = (List<?>) invoke(plan, "steps");
        check(invoke(plan, "paneCount").equals(3) && steps.size() == 2, "Java rebuild plan");
        check(invoke(steps.get(0), "sourceLeafId").equals(0)
            && invoke(steps.get(0), "newLeafId").equals(1)
            && ALTERNATE.equals(invoke(steps.get(0), "connectionId")), "alternate SSH in Java rebuild plan");
        check(invoke(steps.get(1), "sourceLeafId").equals(1)
            && invoke(steps.get(1), "newLeafId").equals(2)
            && invoke(steps.get(1), "connectionId") == null, "inherited primary SSH in Java rebuild plan");

        var second = windows.get(1);
        UUID.fromString(second.getWindowId());
        geometry = second.getGeometry();
        check(geometry.getWidth() == 700 && geometry.getHeight() == 520
            && geometry.getX() == 980 && geometry.getY() == 95, "second window geometry");
        check(second.getTabs().size() == 2 && second.getActiveTabIndex() == 1
            && second.getActiveSessionId().equals(second.getTabs().get(1).getSessionId()), "second window tabs");
        check(second.getTabs().get(0).getConnectionId().equals(SERIAL), "serial tab link");

        var connections = new XMLConnectionRepository(directory).loadConnections();
        var endpoint = connection(connections, PRIMARY);
        var jump = endpoint.getJumpServer();
        check(endpoint.getHost().equals("edge.example") && endpoint.getPort() == 2222
            && endpoint.getUsername().equals("operator"), "primary SSH route");
        check(jump.isEnabled() && jump.getHost().equals("gateway.example") && jump.getPort() == 2200
            && jump.getUsername().equals("relay"), "jump route");
        var alternate = connection(connections, ALTERNATE);
        check(alternate.getHost().equals("other.example") && alternate.getPort() == 22
            && alternate.getUsername().equals("ops"), "alternate SSH route");
        var serial = connection(connections, SERIAL).getSerialSettings();
        check(serial.getPortName().equals("/dev/ttyUSB9") && serial.getBaudRate() == 115200
            && serial.getDataBits() == 7 && serial.getParity().name().equals("EVEN")
            && serial.getStopBits().name().equals("TWO") && !serial.isAssertDtr()
            && serial.isLocalEcho() && serial.getLineEnding().name().equals("CRLF"), "serial settings");
        System.out.println("pinned Java loader and split restore plan accepted the Rust project");
    }
}
