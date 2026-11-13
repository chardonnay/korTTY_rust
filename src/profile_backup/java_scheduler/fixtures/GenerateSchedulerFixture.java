import de.kortty.jobscheduler.JobActionType;
import de.kortty.jobscheduler.JobDependencyMode;
import de.kortty.jobscheduler.JobSchedule;
import de.kortty.jobscheduler.JobSchedulerRepository.SchedulerData;
import de.kortty.jobscheduler.ScheduledJob;
import de.kortty.jobscheduler.SudoCredential;
import jakarta.xml.bind.JAXBContext;
import jakarta.xml.bind.Marshaller;

import java.nio.file.Path;
import java.time.DayOfWeek;
import java.time.LocalTime;
import java.util.List;

public class GenerateSchedulerFixture {
    private static final String COMMAND_ID = "3f9bab16-184b-4e13-bf71-e37331b8843a";
    private static final String TARGET_ID = "b04d413f-9ee5-4b56-bc12-c72da52d4b3e";

    public static void main(String[] args) throws Exception {
        var command = job(COMMAND_ID, "Inspect café & 東京");
        command.setConnectionId(TARGET_ID);
        command.setWorkingDirectory("/srv/project files");
        command.getAction().setCommand("printf '%s\\n' 'fixture & data'");
        var schedule = new JobSchedule();
        schedule.setWeekdaysFromDays(List.of(DayOfWeek.MONDAY, DayOfWeek.FRIDAY));
        schedule.setFixedTimesFromLocalTimes(List.of(LocalTime.of(22, 15), LocalTime.of(0, 15)));
        schedule.setActiveFromDate("2026-11-01");
        schedule.setActiveUntilDate("2027-02-01");
        schedule.setWindowStartTime("22:00");
        schedule.setWindowEndTime("02:00");
        schedule.setIntervalMinutes(30);
        command.setSchedule(schedule);

        var snippet = job("0bd37347-4202-4c5a-912d-4fe43c09f755", "Follow up");
        snippet.setTargetConnectionIds(List.of(TARGET_ID, "9c0719d8-dfa7-4468-a1d9-4149cb8fa01e"));
        snippet.setDependsOnJobIds(List.of(COMMAND_ID));
        snippet.setDependencyMode(JobDependencyMode.CHAIN);
        snippet.setDependencyMaxAgeMinutes(35);
        snippet.getAction().setType(JobActionType.SNIPPET_SCRIPT);
        snippet.getAction().setSnippetId("3ee66545-2eaf-4ac7-b352-d9a1a51d75ed");
        snippet.getAction().setSnippetArguments(List.of("literal space", "a'b"));

        var privileged = job("e02d8f33-ccdb-4fd8-bd8c-340688f038a3", "Privileged command");
        privileged.setConnectionId(TARGET_ID);
        privileged.getAction().setCommand("whoami");
        privileged.getAction().setUseSudo(true);

        var missingParent = job("50dc55a9-cac9-4798-9329-08798863715b", "Depends on sudo");
        missingParent.setConnectionId(TARGET_ID);
        missingParent.getAction().setCommand("true");
        missingParent.setDependsOnJobIds(List.of(privileged.getId()));

        var grouped = job("7ec12ac5-5d6e-496b-947c-deb608c9f11d", "Group selection");
        grouped.setTargetGroupNames(List.of("Ops/Hosts"));
        grouped.getAction().setCommand("uptime");

        var agent = job("2bd70fbe-a5e4-4b29-b621-370fcce50b96", "Agent task");
        agent.setConnectionId(TARGET_ID);
        agent.getAction().setType(JobActionType.AI_AGENT);
        agent.getAction().setAiPrompt("Inspect the fixture");
        agent.getAction().setAiAutoApproveCommands(true);

        var secret = new SudoCredential();
        secret.setId("e5c4e5c9-8d42-4f16-9391-3ec17270c584");
        secret.setServerConnectionId(TARGET_ID);
        secret.setEncryptedPassword("synthetic-sudo-ciphertext");
        secret.setUpdatedAt("2026-11-13T08:15:00Z");

        var data = new SchedulerData();
        var jobs = List.of(command, snippet, privileged, missingParent, grouped, agent);
        for (var job : jobs) {
            job.setCreatedAt("2026-11-13T08:15:00Z");
            job.setUpdatedAt("2026-11-13T08:15:00Z");
        }
        data.setJobs(jobs);
        data.setSudoCredentials(List.of(secret));
        var marshaller = JAXBContext.newInstance(SchedulerData.class).createMarshaller();
        marshaller.setProperty(Marshaller.JAXB_FORMATTED_OUTPUT, true);
        marshaller.marshal(data, Path.of(args[0]).toFile());
    }

    private static ScheduledJob job(String id, String name) {
        var job = new ScheduledJob();
        job.setId(id);
        job.setName(name);
        return job;
    }
}
