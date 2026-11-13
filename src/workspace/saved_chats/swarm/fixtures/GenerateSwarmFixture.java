import de.kortty.core.SwarmChatManager.SwarmChatsWrapper;
import de.kortty.model.SavedSwarmChat;
import de.kortty.model.SavedSwarmMessage;
import de.kortty.model.SavedSwarmServerSummary;
import jakarta.xml.bind.JAXBContext;
import jakarta.xml.bind.Marshaller;

import java.nio.file.Path;
import java.util.List;

public class GenerateSwarmFixture {
    public static void main(String[] args) throws Exception {
        long created = 1720000000000L;
        var chat = new SavedSwarmChat();
        chat.setId("9012ae78-adc2-4301-8ffe-c59163856c4d");
        chat.setTitle("Grüße & 東京");
        chat.setCreatedAt(created);
        chat.setUpdatedAt(created + 3000);
        chat.setResponseLanguageCode("de-DE");
        chat.setActiveAiProfileId("77");
        chat.setActiveAiProfileName("Synthetic Java provider");
        chat.setTargetConnectionIds(List.of("42", "88"));
        chat.setTargetGroupPaths(List.of("synthetic-private-group"));

        var prompt = message("USER", "Inspect both targets.", created + 1000);
        var response = message("ASSISTANT", "# Combined result\n\nTwo targets reported synthetic-private-token.", created + 2000);
        response.setAiProfileId("77");
        response.setAiProfileName("Fixture model");
        response.setServerSummaries(List.of(
            server("München & 1", "DONE", "Completed successfully.", 17, 1205),
            server("東京 2", "SKIPPED", "synthetic-private-token", 0, 0)));
        var incomplete = message("USER", "Check again?", created + 3000);
        chat.setMessages(List.of(prompt, response, incomplete));

        var wrapper = new SwarmChatsWrapper();
        wrapper.setChats(List.of(chat));
        var marshaller = JAXBContext.newInstance(SwarmChatsWrapper.class, SavedSwarmChat.class,
            SavedSwarmMessage.class, SavedSwarmServerSummary.class).createMarshaller();
        marshaller.setProperty(Marshaller.JAXB_FORMATTED_OUTPUT, true);
        marshaller.marshal(wrapper, Path.of(args[0]).toFile());
    }

    private static SavedSwarmMessage message(String role, String content, long created) {
        var message = new SavedSwarmMessage();
        message.setRole(role);
        message.setContent(content);
        message.setCreatedAt(created);
        return message;
    }

    private static SavedSwarmServerSummary server(String name, String state, String summary,
                                                   long seconds, long tokens) {
        var server = new SavedSwarmServerSummary();
        server.setServerDisplayName(name);
        server.setFinalState(state);
        server.setSummaryText(summary);
        server.setElapsedSeconds(seconds);
        server.setTotalTokens(tokens);
        return server;
    }
}
