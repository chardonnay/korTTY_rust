use crate::model::connection::{AuthMethod, ConnectionProtocol, ConnectionSettings};
use crate::model::ssh_key::SSHKey;
use crate::persistence::xml_repository;
use anyhow::Result;
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use russh::*;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

pub struct SSHSession {
    pub connection_id: String,
    pub settings: ConnectionSettings,
    mode: SessionMode,
    output_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    exec_outputs: Arc<std::sync::Mutex<HashMap<ChannelId, ExecChannelOutput>>>,
    runtime_state: Arc<std::sync::Mutex<TerminalRuntimeState>>,
}

#[derive(Clone)]
struct ExecChannelOutput {
    stdout_tx: mpsc::UnboundedSender<Vec<u8>>,
    stderr_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

enum SessionMode {
    Russh {
        handle: Option<client::Handle<SSHHandler>>,
        channel: Option<Channel<client::Msg>>,
    },
    Mosh {
        state: Arc<std::sync::Mutex<MoshState>>,
    },
}

struct MoshState {
    child: Option<Box<dyn portable_pty::Child + Send>>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
}

struct SSHHandler {
    output_tx: mpsc::UnboundedSender<Vec<u8>>,
    exec_outputs: Arc<std::sync::Mutex<HashMap<ChannelId, ExecChannelOutput>>>,
    runtime_state: Arc<std::sync::Mutex<TerminalRuntimeState>>,
}

#[derive(Debug, Clone)]
struct TerminalRuntimeState {
    current_remote_directory: String,
    home_remote_directory: String,
    previous_remote_directory: String,
    directory_stack: VecDeque<String>,
    input_line_buffer: String,
    osc7_buffer: String,
    agent_osc_buffer: String,
}

impl Default for TerminalRuntimeState {
    fn default() -> Self {
        Self {
            current_remote_directory: "~".into(),
            home_remote_directory: "~".into(),
            previous_remote_directory: "~".into(),
            directory_stack: VecDeque::new(),
            input_line_buffer: String::new(),
            osc7_buffer: String::new(),
            agent_osc_buffer: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalExecOutputKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct TerminalExecOutput {
    pub kind: TerminalExecOutputKind,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct TerminalExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: Option<u32>,
    pub exit_signal: Option<String>,
    pub cancelled: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecDeadlineEvent {
    CancelRequested,
    DeadlineElapsed,
}

#[derive(Debug, Clone)]
enum ExecDeadlineAction {
    Continue,
    Break,
    RequestStop { signal: Sig },
}

impl client::Handler for SSHHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let exec_outputs = lock_mutex(&self.exec_outputs, "SSH exec output registry")?;
        if let Some(output) = exec_outputs.get(&channel) {
            let _ = output.stdout_tx.send(data.to_vec());
        } else {
            update_current_directory_from_output(&self.runtime_state, data);
            let _ = self.output_tx.send(data.to_vec());
        }
        Ok(())
    }

    async fn extended_data(
        &mut self,
        channel: ChannelId,
        ext: u32,
        data: &[u8],
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let exec_outputs = lock_mutex(&self.exec_outputs, "SSH exec output registry")?;
        if let Some(output) = exec_outputs.get(&channel) {
            if ext == 1 {
                if let Some(stderr_tx) = &output.stderr_tx {
                    let _ = stderr_tx.send(data.to_vec());
                } else {
                    let _ = output.stdout_tx.send(data.to_vec());
                }
            } else {
                let _ = output.stdout_tx.send(data.to_vec());
            }
        } else {
            update_current_directory_from_output(&self.runtime_state, data);
            let _ = self.output_tx.send(data.to_vec());
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        lock_mutex(&self.exec_outputs, "SSH exec output registry")?.remove(&channel);
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        lock_mutex(&self.exec_outputs, "SSH exec output registry")?.remove(&channel);
        Ok(())
    }
}

impl SSHSession {
    pub fn new(settings: ConnectionSettings) -> Self {
        let mode = match settings.connection_protocol {
            ConnectionProtocol::Mosh => SessionMode::Mosh {
                state: Arc::new(std::sync::Mutex::new(MoshState {
                    child: None,
                    master: None,
                    writer: None,
                })),
            },
            ConnectionProtocol::TcpIp => SessionMode::Russh {
                handle: None,
                channel: None,
            },
        };
        Self {
            connection_id: settings.id.clone(),
            settings,
            mode,
            output_tx: None,
            exec_outputs: Arc::new(std::sync::Mutex::new(HashMap::new())),
            runtime_state: Arc::new(std::sync::Mutex::new(TerminalRuntimeState::default())),
        }
    }

    pub async fn connect(&mut self, output_tx: mpsc::UnboundedSender<Vec<u8>>) -> Result<()> {
        self.output_tx = Some(output_tx.clone());

        match self.settings.connection_protocol {
            ConnectionProtocol::TcpIp => self.connect_ssh(output_tx).await,
            ConnectionProtocol::Mosh => self.connect_mosh(output_tx).await,
        }
    }

    async fn connect_ssh(&mut self, output_tx: mpsc::UnboundedSender<Vec<u8>>) -> Result<()> {
        let resolved_private_key = match self.settings.auth_method {
            AuthMethod::PrivateKey => Some(self.resolve_private_key()?),
            AuthMethod::Password => None,
        };

        let (handle_ref, channel_ref) = match &mut self.mode {
            SessionMode::Russh { handle, channel } => (handle, channel),
            _ => anyhow::bail!("Invalid session mode for SSH connection"),
        };

        let config = Arc::new(client::Config {
            ..Default::default()
        });

        let handler = SSHHandler {
            output_tx: output_tx.clone(),
            exec_outputs: self.exec_outputs.clone(),
            runtime_state: self.runtime_state.clone(),
        };

        let addr = format!("{}:{}", self.settings.host, self.settings.port);
        let mut handle = client::connect(config, &addr, handler).await?;

        match &self.settings.auth_method {
            AuthMethod::Password => {
                let password = self.settings.password.clone().unwrap_or_default();
                let authenticated = handle
                    .authenticate_password(&self.settings.username, &password)
                    .await?;
                if !authenticated.success() {
                    anyhow::bail!("Password authentication failed");
                }
            }
            AuthMethod::PrivateKey => {
                let key_pair = resolved_private_key
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Missing private key"))?;
                let authenticated = handle
                    .authenticate_publickey(
                        &self.settings.username,
                        russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), None),
                    )
                    .await?;
                if !authenticated.success() {
                    anyhow::bail!("Private key authentication failed");
                }
            }
        }

        let channel = handle.channel_open_session().await?;

        let term = "xterm-256color";
        let cols = self.settings.columns as u32;
        let rows = self.settings.rows as u32;
        let terminal_modes = Vec::new();
        channel
            .request_pty(false, term, cols, rows, 0, 0, &terminal_modes)
            .await?;
        channel.request_shell(false).await?;
        if self.settings.prompt_hook_enabled {
            if let Some(startup_command) = build_terminal_agent_shell_startup_command(
                self.settings.terminal_agent_command_name.as_deref(),
            ) {
                channel.data(startup_command.as_bytes()).await?;
            }
        }

        *handle_ref = Some(handle);
        *channel_ref = Some(channel);

        Ok(())
    }

    async fn connect_mosh(&mut self, output_tx: mpsc::UnboundedSender<Vec<u8>>) -> Result<()> {
        let state = match &self.mode {
            SessionMode::Mosh { state } => state.clone(),
            _ => anyhow::bail!("Invalid session mode for MOSH connection"),
        };

        let cols = self.settings.columns;
        let rows = self.settings.rows;
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new("mosh");
        match self.settings.auth_method {
            AuthMethod::Password => {
                let password = self.settings.password.clone().unwrap_or_default();
                if password.is_empty() {
                    anyhow::bail!("MOSH password auth selected, but no password is configured");
                }

                if !is_command_available("sshpass") {
                    anyhow::bail!(
                        "MOSH password auth requires 'sshpass'. Please install sshpass or switch to key auth."
                    );
                }

                let ssh_cmd = if self.settings.port != 22 {
                    format!("sshpass -e ssh -p {}", self.settings.port)
                } else {
                    "sshpass -e ssh".to_string()
                };
                cmd.arg("--ssh");
                cmd.arg(ssh_cmd);
                cmd.env("SSHPASS", password);
            }
            AuthMethod::PrivateKey => {
                if self.settings.port != 22 {
                    cmd.arg("--ssh");
                    cmd.arg(format!("ssh -p {}", self.settings.port));
                }
            }
        }
        cmd.arg(format!("{}@{}", self.settings.username, self.settings.host));

        let child = pair.slave.spawn_command(cmd)?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let tx = output_tx.clone();
        let runtime_state = self.runtime_state.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        update_current_directory_from_output(&runtime_state, &buf[..n]);
                        let _ = tx.send(buf[..n].to_vec());
                    }
                    Err(_) => break,
                }
            }
        });

        let mut locked = lock_mutex(&state, "MOSH session state")?;
        locked.child = Some(child);
        locked.writer = Some(writer);
        locked.master = Some(pair.master);

        Ok(())
    }

    fn register_exec_output(
        &self,
        channel_id: ChannelId,
        stdout_tx: mpsc::UnboundedSender<Vec<u8>>,
        stderr_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    ) -> Result<()> {
        lock_mutex(&self.exec_outputs, "SSH exec output registry")?.insert(
            channel_id,
            ExecChannelOutput {
                stdout_tx,
                stderr_tx,
            },
        );
        Ok(())
    }

    fn remove_exec_output(&self, channel_id: ChannelId) {
        let _ = lock_mutex(&self.exec_outputs, "SSH exec output registry")
            .map(|mut registry| registry.remove(&channel_id));
    }

    pub async fn exec_command_streaming(
        &self,
        command: &str,
        output_tx: mpsc::UnboundedSender<TerminalExecOutput>,
        mut cancel_rx: watch::Receiver<bool>,
        timeout: std::time::Duration,
        stdin_data: Option<Vec<u8>>,
        request_pty: bool,
    ) -> Result<TerminalExecResult> {
        let handle = match &self.mode {
            SessionMode::Russh { handle, .. } => handle
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not connected"))?,
            SessionMode::Mosh { .. } => {
                anyhow::bail!("exec_command_streaming is not supported for MOSH sessions");
            }
        };

        let mut channel = handle.channel_open_session().await?;
        let channel_id = channel.id();
        let (stdout_tx, mut stdout_rx) = mpsc::unbounded_channel();
        let (stderr_tx, mut stderr_rx) = mpsc::unbounded_channel();
        self.register_exec_output(channel_id, stdout_tx, Some(stderr_tx))?;
        if request_pty {
            let cols = self.settings.columns as u32;
            let rows = self.settings.rows as u32;
            channel
                .request_pty(false, "xterm-256color", cols, rows, 0, 0, &[])
                .await?;
        }
        channel.exec(true, command).await?;
        if let Some(stdin_data) = stdin_data.as_ref() {
            channel.data(stdin_data.as_slice()).await?;
            channel.eof().await?;
        }

        let mut result = TerminalExecResult::default();
        let mut wait_deadline = Box::pin(tokio::time::sleep_until(
            tokio::time::Instant::now() + timeout,
        ));
        let mut stop_requested = false;
        let mut stdout_open = true;
        let mut stderr_open = true;
        let stop_grace = std::time::Duration::from_secs(2);

        loop {
            if !stdout_open && !stderr_open {
                break;
            }
            tokio::select! {
                maybe_stdout = stdout_rx.recv(), if stdout_open => {
                    match maybe_stdout {
                        Some(bytes) => {
                            let text = String::from_utf8_lossy(&bytes).to_string();
                            result.stdout.push_str(&text);
                            let _ = output_tx.send(TerminalExecOutput {
                                kind: TerminalExecOutputKind::Stdout,
                                text,
                            });
                        }
                        None => {
                            stdout_open = false;
                            if stop_requested && !stderr_open {
                                break;
                            }
                        }
                    }
                }
                maybe_stderr = stderr_rx.recv(), if stderr_open => {
                    match maybe_stderr {
                        Some(bytes) => {
                            let text = String::from_utf8_lossy(&bytes).to_string();
                            result.stderr.push_str(&text);
                            let _ = output_tx.send(TerminalExecOutput {
                                kind: TerminalExecOutputKind::Stderr,
                                text,
                            });
                        }
                        None => {
                            stderr_open = false;
                            if stop_requested && !stdout_open {
                                break;
                            }
                        }
                    }
                }
                maybe_message = channel.wait() => {
                    match maybe_message {
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            result.exit_status = Some(exit_status);
                        }
                        Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                            result.exit_signal = Some(format!("{signal_name:?}"));
                        }
                        Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    }
                }
                changed = cancel_rx.changed(), if !stop_requested => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        match classify_exec_deadline_event(
                            &mut result,
                            stop_requested,
                            ExecDeadlineEvent::CancelRequested,
                        ) {
                            ExecDeadlineAction::RequestStop { signal } => {
                                let _ = channel.signal(signal).await;
                                let _ = channel.eof().await;
                                let _ = channel.close().await;
                                stop_requested = true;
                                wait_deadline
                                    .as_mut()
                                    .reset(tokio::time::Instant::now() + stop_grace);
                            }
                            ExecDeadlineAction::Break => break,
                            ExecDeadlineAction::Continue => {}
                        }
                    }
                }
                _ = &mut wait_deadline => {
                    match classify_exec_deadline_event(
                        &mut result,
                        stop_requested,
                        ExecDeadlineEvent::DeadlineElapsed,
                    ) {
                        ExecDeadlineAction::RequestStop { signal } => {
                            let _ = channel.signal(signal).await;
                            let _ = channel.eof().await;
                            let _ = channel.close().await;
                            stop_requested = true;
                            wait_deadline
                                .as_mut()
                                .reset(tokio::time::Instant::now() + stop_grace);
                        }
                        ExecDeadlineAction::Break => break,
                        ExecDeadlineAction::Continue => {}
                    }
                }
            }
        }

        self.remove_exec_output(channel_id);

        while let Ok(bytes) = stdout_rx.try_recv() {
            let text = String::from_utf8_lossy(&bytes).to_string();
            result.stdout.push_str(&text);
            let _ = output_tx.send(TerminalExecOutput {
                kind: TerminalExecOutputKind::Stdout,
                text,
            });
        }

        while let Ok(bytes) = stderr_rx.try_recv() {
            let text = String::from_utf8_lossy(&bytes).to_string();
            result.stderr.push_str(&text);
            let _ = output_tx.send(TerminalExecOutput {
                kind: TerminalExecOutputKind::Stderr,
                text,
            });
        }

        Ok(result)
    }

    pub async fn exec_command(&self, command: &str) -> Result<String> {
        let handle = match &self.mode {
            SessionMode::Russh { handle, .. } => handle
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not connected"))?,
            SessionMode::Mosh { .. } => {
                anyhow::bail!("exec_command is not supported for MOSH sessions");
            }
        };

        let channel = handle.channel_open_session().await?;
        let channel_id = channel.id();

        let (tx, mut rx) = mpsc::unbounded_channel();
        self.register_exec_output(channel_id, tx, None)?;

        channel.exec(true, command).await?;

        let mut output = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);

        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(data)) => output.extend(data),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        self.remove_exec_output(channel_id);
        drop(channel);

        String::from_utf8(output)
            .map_err(|error| anyhow::anyhow!("command output is not valid UTF-8: {error}"))
    }

    pub async fn send_data(&mut self, data: &[u8]) -> Result<()> {
        track_potential_directory_change(&self.runtime_state, data);
        match &mut self.mode {
            SessionMode::Russh { channel, .. } => {
                if let Some(channel) = channel {
                    channel.data(data).await?;
                }
            }
            SessionMode::Mosh { state } => {
                let mut locked = lock_mutex(state, "MOSH session state")?;
                if let Some(writer) = &mut locked.writer {
                    writer.write_all(data)?;
                    writer.flush()?;
                }
            }
        }
        Ok(())
    }

    pub fn current_remote_directory(&self) -> String {
        self.runtime_state
            .lock()
            .map(|state| state.current_remote_directory.clone())
            .unwrap_or_else(|_| "~".into())
    }

    pub async fn resize(&mut self, cols: u32, rows: u32) -> Result<()> {
        match &mut self.mode {
            SessionMode::Russh { channel, .. } => {
                if let Some(channel) = channel {
                    channel.window_change(cols, rows, 0, 0).await?;
                }
            }
            SessionMode::Mosh { state } => {
                let mut locked = lock_mutex(state, "MOSH session state")?;
                if let Some(master) = &mut locked.master {
                    master.resize(PtySize {
                        rows: rows as u16,
                        cols: cols as u16,
                        pixel_width: 0,
                        pixel_height: 0,
                    })?;
                }
            }
        }
        Ok(())
    }

    pub async fn disconnect(&mut self) -> Result<()> {
        match &mut self.mode {
            SessionMode::Russh { handle, channel } => {
                if let Some(h) = handle.take() {
                    let _ = h
                        .disconnect(Disconnect::ByApplication, "User disconnected", "")
                        .await;
                }
                *channel = None;
            }
            SessionMode::Mosh { state } => {
                let mut locked = lock_mutex(state, "MOSH session state")?;
                if let Some(c) = locked.child.as_mut() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                locked.child = None;
                locked.writer = None;
                locked.master = None;
            }
        }
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        match &self.mode {
            SessionMode::Russh { handle, .. } => handle.is_some(),
            SessionMode::Mosh { state } => match state.lock() {
                Ok(locked) => locked.child.is_some(),
                Err(_) => false,
            },
        }
    }

    fn resolve_private_key(&self) -> Result<russh::keys::PrivateKey> {
        // Temporary key content has highest priority and behaves exactly like Java's TEMPORARY: flow.
        let temporary_content = self
            .settings
            .temporary_key_content
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                self.settings
                    .private_key_path
                    .as_ref()
                    .and_then(|path| path.strip_prefix("TEMPORARY:"))
                    .map(|s| s.trim().to_string())
            });

        if let Some(mut key_content) = temporary_content {
            if !key_content.contains("-----BEGIN") || !key_content.contains("-----END") {
                anyhow::bail!(
                    "Temporary SSH key is incomplete. A full private key with BEGIN/END markers is required."
                );
            }
            if !key_content.ends_with('\n') {
                key_content.push('\n');
            }
            let passphrase = self
                .settings
                .private_key_passphrase
                .as_deref()
                .filter(|s| !s.trim().is_empty());
            return russh::keys::decode_secret_key(&key_content, passphrase)
                .map_err(|e| anyhow::anyhow!("Failed to decode temporary SSH key: {e}"));
        }

        // Resolve key path either from selected SSH key ID or from direct path.
        let mut resolved_path = self
            .settings
            .private_key_path
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let mut resolved_passphrase = self
            .settings
            .private_key_passphrase
            .clone()
            .filter(|s| !s.trim().is_empty());

        if let Some(ssh_key_id) = self
            .settings
            .ssh_key_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            let keys: Vec<SSHKey> = xml_repository::load_json("ssh-keys.json")?.unwrap_or_default();
            if let Some(found) = keys.iter().find(|k| k.id == ssh_key_id) {
                if !found.path.trim().is_empty() {
                    resolved_path = Some(found.path.trim().to_string());
                }
                if resolved_passphrase.is_none() {
                    resolved_passphrase = found
                        .encrypted_passphrase
                        .clone()
                        .filter(|s| !s.trim().is_empty());
                }
            }
        }

        let path = resolved_path.ok_or_else(|| {
            anyhow::anyhow!("Private key auth selected, but neither temporary key content nor key path is configured")
        })?;

        let expanded_path = if let Some(rest) = path.strip_prefix("~/") {
            let home =
                dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot resolve home directory"))?;
            home.join(rest).to_string_lossy().into_owned()
        } else {
            path
        };

        russh::keys::load_secret_key(
            expanded_path,
            resolved_passphrase
                .as_deref()
                .filter(|s| !s.trim().is_empty()),
        )
        .map_err(|e| anyhow::anyhow!("Failed to load private key: {e}"))
    }
}

fn build_terminal_agent_shell_startup_command(command_name: Option<&str>) -> Option<String> {
    let command_names = terminal_agent_shell_alias_names(command_name);
    if command_names.is_empty() {
        return None;
    }

    let alias_commands = command_names
        .iter()
        .map(|command| {
            let ask_command = format!("{command}-ask");
            let plan_command = format!("{command}-plan");
            format!(
                "alias {command}='__kortty_agent_emit execute'; \
alias {ask_command}='__kortty_agent_emit ask'; \
alias {plan_command}='__kortty_agent_emit plan'; "
            )
        })
        .collect::<String>();

    Some(format!(
        "__kortty_agent_b64(){{ if command -v base64 >/dev/null 2>&1; then \
printf '%s' \"$*\" | base64 | tr -d '\\r\\n'; \
elif command -v python3 >/dev/null 2>&1; then \
python3 -c 'import base64,sys;print(base64.b64encode(\" \".join(sys.argv[1:]).encode()).decode(), end=\"\")' \"$@\"; \
else printf ''; fi; }}; \
__kortty_agent_emit(){{ __kortty_kind=$1; shift; \
__kortty_cwd=$(pwd -P 2>/dev/null || pwd 2>/dev/null || printf ''); \
__kortty_cwd_payload=$(__kortty_agent_b64 \"$__kortty_cwd\"); \
__kortty_payload=$(__kortty_agent_b64 \"$@\"); \
printf '\\033]777;korTTY-agent;%s;%s;%s\\007' \"$__kortty_kind\" \"$__kortty_cwd_payload\" \"$__kortty_payload\"; }}; \
{alias_commands}\
__kortty_agent_clean_history(){{ if [ -n \"${{BASH_VERSION-}}\" ]; then \
if command -v awk >/dev/null 2>&1 && command -v sort >/dev/null 2>&1; then \
for __kortty_h in $(history | awk '/__kortty_agent_b64\\(\\)/ {{print $1}}' | sort -rn); do \
history -d \"$__kortty_h\" 2>/dev/null || true; done; \
else history -d $((HISTCMD-1)) 2>/dev/null || true; fi; \
if [ -n \"${{HISTFILE-}}\" ] && [ -f \"$HISTFILE\" ] && [ -w \"$HISTFILE\" ] && command -v awk >/dev/null 2>&1; then \
__kortty_hist_tmp=\"${{HISTFILE}}.kortty.$$\"; \
awk 'index($0,\"__kortty_agent_b64(){{\")==0' \"$HISTFILE\" > \"$__kortty_hist_tmp\" \
&& cat \"$__kortty_hist_tmp\" > \"$HISTFILE\"; rm -f \"$__kortty_hist_tmp\"; fi; \
fi; }}; \
__kortty_agent_clean_history; unset -f __kortty_agent_clean_history; \
printf '\\033[1A\\r\\033[K'; \
stty echo\n"
    ))
}

fn terminal_agent_shell_alias_names(command_name: Option<&str>) -> Vec<String> {
    let normalized_command = normalize_terminal_agent_command_name(command_name);
    let mut command_names = Vec::new();
    if is_valid_terminal_agent_command_name(&normalized_command) {
        command_names.push(normalized_command);
    }
    if !command_names.iter().any(|command| command == "agent") {
        command_names.push("agent".to_string());
    }
    command_names
}

fn normalize_terminal_agent_command_name(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("agent")
        .to_string()
}

fn is_valid_terminal_agent_command_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn update_current_directory_from_output(
    runtime_state: &Arc<std::sync::Mutex<TerminalRuntimeState>>,
    data: &[u8],
) {
    let text = String::from_utf8_lossy(data);
    if text.is_empty() {
        return;
    }
    let Ok(mut state) = runtime_state.lock() else {
        return;
    };
    process_agent_osc(&mut state, &text);
    process_osc7(&mut state, &text);
}

fn process_agent_osc(state: &mut TerminalRuntimeState, text: &str) {
    state.agent_osc_buffer.push_str(text);
    loop {
        const PREFIX: &str = "\u{1b}]777;korTTY-agent;";
        let Some(start) = state.agent_osc_buffer.find(PREFIX) else {
            trim_buffer(&mut state.agent_osc_buffer, 4096);
            return;
        };
        let tail = &state.agent_osc_buffer[start..];
        let bell_end = tail.find('\u{7}').map(|index| start + index);
        let st_end = tail.find("\u{1b}\\").map(|index| start + index);
        let (end, terminator_len) = match (bell_end, st_end) {
            (Some(bell), Some(st)) if bell < st => (bell, 1),
            (Some(bell), None) => (bell, 1),
            (_, Some(st)) => (st, 2),
            (None, None) => {
                if start > 0 {
                    state.agent_osc_buffer.drain(..start);
                }
                trim_buffer(&mut state.agent_osc_buffer, 4096);
                return;
            }
        };
        let payload_start = start + PREFIX.len();
        let payload = state.agent_osc_buffer[payload_start..end].to_string();
        state.agent_osc_buffer.drain(..end + terminator_len);
        if let Some(cwd) = extract_working_directory_from_agent_osc_payload(&payload) {
            set_current_remote_directory(state, &cwd);
        }
    }
}

fn process_osc7(state: &mut TerminalRuntimeState, text: &str) {
    state.osc7_buffer.push_str(text);
    loop {
        const PREFIX: &str = "\u{1b}]7;";
        let Some(start) = state.osc7_buffer.find(PREFIX) else {
            trim_buffer(&mut state.osc7_buffer, 4096);
            return;
        };
        let tail = &state.osc7_buffer[start..];
        let bell_end = tail.find('\u{7}').map(|index| start + index);
        let st_end = tail.find("\u{1b}\\").map(|index| start + index);
        let (end, terminator_len) = match (bell_end, st_end) {
            (Some(bell), Some(st)) if bell < st => (bell, 1),
            (Some(bell), None) => (bell, 1),
            (_, Some(st)) => (st, 2),
            (None, None) => {
                if start > 0 {
                    state.osc7_buffer.drain(..start);
                }
                trim_buffer(&mut state.osc7_buffer, 4096);
                return;
            }
        };
        let payload_start = start + PREFIX.len();
        let payload = state.osc7_buffer[payload_start..end].to_string();
        state.osc7_buffer.drain(..end + terminator_len);
        if let Some(path) = extract_path_from_osc7_payload(&payload) {
            set_current_remote_directory(state, &path);
        }
    }
}

fn extract_working_directory_from_agent_osc_payload(payload: &str) -> Option<String> {
    let mut parts = payload.splitn(3, ';');
    let _kind = parts.next()?;
    let encoded_cwd = parts.next()?;
    let cwd = base64::engine::general_purpose::STANDARD
        .decode(encoded_cwd)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())?;
    let trimmed = cwd.trim();
    trimmed.starts_with('/').then(|| trimmed.to_string())
}

fn extract_path_from_osc7_payload(payload: &str) -> Option<String> {
    let raw = payload.strip_prefix("file://")?;
    let path_start = raw.find('/')?;
    let decoded = percent_decode(&raw[path_start..]);
    decoded.starts_with('/').then_some(decoded)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(decoded) = u8::from_str_radix(hex, 16) {
                    output.push(decoded);
                    index += 3;
                    continue;
                }
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn track_potential_directory_change(
    runtime_state: &Arc<std::sync::Mutex<TerminalRuntimeState>>,
    data: &[u8],
) {
    let text = String::from_utf8_lossy(data);
    if text.is_empty() {
        return;
    }
    let Ok(mut state) = runtime_state.lock() else {
        return;
    };
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            let input_line = std::mem::take(&mut state.input_line_buffer);
            process_input_line(&mut state, &input_line);
        } else if ch == '\u{8}' || ch == '\u{7f}' {
            state.input_line_buffer.pop();
        } else if !ch.is_control() {
            state.input_line_buffer.push(ch);
        }
    }
    trim_buffer(&mut state.input_line_buffer, 2048);
}

fn process_input_line(state: &mut TerminalRuntimeState, input_line: &str) {
    let segment = first_command_segment(input_line);
    if segment == "cd" || segment.starts_with("cd ") {
        apply_cd_command(state, &segment);
    } else if segment == "pushd" || segment.starts_with("pushd ") {
        apply_pushd_command(state, &segment);
    } else if segment == "popd" {
        apply_popd_command(state);
    }
}

fn first_command_segment(input_line: &str) -> String {
    let mut output = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for ch in input_line.chars() {
        if escaped {
            output.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            output.push(ch);
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            output.push(ch);
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            output.push(ch);
            continue;
        }
        if !in_single && !in_double && (ch == ';' || ch == '&' || ch == '|') {
            break;
        }
        output.push(ch);
    }
    output.trim().to_string()
}

fn apply_cd_command(state: &mut TerminalRuntimeState, command: &str) {
    let target = command
        .strip_prefix("cd")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(unquote)
        .unwrap_or_else(|| state.home_remote_directory.clone());
    let resolved = resolve_remote_path(state, &target);
    set_current_remote_directory(state, &resolved);
}

fn apply_pushd_command(state: &mut TerminalRuntimeState, command: &str) {
    state
        .directory_stack
        .push_back(state.current_remote_directory.clone());
    apply_cd_command(state, command.replacen("pushd", "cd", 1).as_str());
}

fn apply_popd_command(state: &mut TerminalRuntimeState) {
    if let Some(path) = state.directory_stack.pop_back() {
        set_current_remote_directory(state, &path);
    }
}

fn resolve_remote_path(state: &TerminalRuntimeState, path: &str) -> String {
    let mut value = path.trim().to_string();
    if value == "-" {
        return state.previous_remote_directory.clone();
    }
    if value == "~" {
        return state.home_remote_directory.clone();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        value = format!(
            "{}/{}",
            state.home_remote_directory.trim_end_matches('/'),
            rest
        );
    } else if !value.starts_with('/') {
        value = format!(
            "{}/{}",
            state.current_remote_directory.trim_end_matches('/'),
            value
        );
    }
    normalize_remote_path(&value)
}

fn normalize_remote_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    let mut result = if absolute {
        "/".to_string()
    } else {
        String::new()
    };
    result.push_str(&parts.join("/"));
    if result.is_empty() {
        if absolute {
            "/".into()
        } else {
            ".".into()
        }
    } else {
        result
    }
}

fn set_current_remote_directory(state: &mut TerminalRuntimeState, path: &str) {
    let normalized = normalize_remote_path(path);
    if normalized.starts_with('/') && normalized != state.current_remote_directory {
        state.previous_remote_directory = state.current_remote_directory.clone();
        state.current_remote_directory = normalized.clone();
        if state.home_remote_directory == "~" {
            state.home_remote_directory = normalized;
        }
    }
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn trim_buffer(value: &mut String, max_len: usize) {
    if value.len() > max_len {
        let split_at = value
            .char_indices()
            .map(|(index, _)| index)
            .find(|index| value.len() - *index <= max_len)
            .unwrap_or(value.len().saturating_sub(max_len));
        value.drain(..split_at);
    }
}

fn lock_mutex<'a, T>(
    mutex: &'a std::sync::Mutex<T>,
    resource_name: &'static str,
) -> Result<std::sync::MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| anyhow::anyhow!("{resource_name} is poisoned"))
}

fn is_command_available(cmd: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {} >/dev/null 2>&1", cmd))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn classify_exec_deadline_event(
    result: &mut TerminalExecResult,
    stop_requested: bool,
    event: ExecDeadlineEvent,
) -> ExecDeadlineAction {
    match (stop_requested, event) {
        (false, ExecDeadlineEvent::CancelRequested) => {
            result.cancelled = true;
            ExecDeadlineAction::RequestStop { signal: Sig::INT }
        }
        (false, ExecDeadlineEvent::DeadlineElapsed) => {
            result.timed_out = true;
            ExecDeadlineAction::RequestStop { signal: Sig::TERM }
        }
        (true, ExecDeadlineEvent::DeadlineElapsed) => ExecDeadlineAction::Break,
        (true, ExecDeadlineEvent::CancelRequested) => ExecDeadlineAction::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn cancel_requests_sigint_and_marks_result_cancelled() {
        let mut result = TerminalExecResult::default();

        let action =
            classify_exec_deadline_event(&mut result, false, ExecDeadlineEvent::CancelRequested);

        assert!(matches!(
            action,
            ExecDeadlineAction::RequestStop { signal: Sig::INT }
        ));
        assert!(result.cancelled);
        assert!(!result.timed_out);
    }

    #[test]
    fn timeout_requests_sigterm_and_marks_result_timed_out() {
        let mut result = TerminalExecResult::default();

        let action =
            classify_exec_deadline_event(&mut result, false, ExecDeadlineEvent::DeadlineElapsed);

        assert!(matches!(
            action,
            ExecDeadlineAction::RequestStop { signal: Sig::TERM }
        ));
        assert!(result.timed_out);
        assert!(!result.cancelled);
    }

    #[test]
    fn second_deadline_after_stop_breaks_exec_loop() {
        let mut result = TerminalExecResult::default();

        let action =
            classify_exec_deadline_event(&mut result, true, ExecDeadlineEvent::DeadlineElapsed);

        assert!(matches!(action, ExecDeadlineAction::Break));
        assert!(!result.cancelled);
        assert!(!result.timed_out);
    }

    #[test]
    fn startup_command_installs_default_agent_alias_trio() {
        let startup = build_terminal_agent_shell_startup_command(None)
            .expect("default agent aliases should be generated");

        assert_startup_has_alias_trio(&startup, "agent");
    }

    #[test]
    fn startup_command_keeps_custom_alias_and_default_agent_alias() {
        let startup = build_terminal_agent_shell_startup_command(Some("Max"))
            .expect("custom and default agent aliases should be generated");

        assert_startup_has_alias_trio(&startup, "Max");
        assert_startup_has_alias_trio(&startup, "agent");
    }

    #[test]
    fn startup_command_falls_back_to_default_agent_for_invalid_custom_alias() {
        let startup = build_terminal_agent_shell_startup_command(Some("not valid"))
            .expect("default agent aliases should be generated");

        assert_startup_has_alias_trio(&startup, "agent");
        assert!(!startup.contains("alias not valid="));
    }

    #[test]
    fn startup_command_does_not_duplicate_agent_aliases() {
        assert_eq!(
            terminal_agent_shell_alias_names(Some("agent")),
            vec!["agent".to_string()]
        );
    }

    #[test]
    fn extracts_cwd_from_osc777_agent_payload() {
        let mut state = TerminalRuntimeState::default();
        let cwd = base64::engine::general_purpose::STANDARD.encode("/tmp/project");
        let prompt = base64::engine::general_purpose::STANDARD.encode("list files");
        process_agent_osc(
            &mut state,
            &format!("\u{1b}]777;korTTY-agent;execute;{cwd};{prompt}\u{7}"),
        );

        assert_eq!(state.current_remote_directory, "/tmp/project");
    }

    #[test]
    fn extracts_cwd_from_osc7_payload() {
        let mut state = TerminalRuntimeState::default();

        process_osc7(&mut state, "\u{1b}]7;file://server/tmp/has%20space\u{7}");

        assert_eq!(state.current_remote_directory, "/tmp/has space");
    }

    #[test]
    fn tracks_typed_directory_changes() {
        let runtime_state = Arc::new(std::sync::Mutex::new(TerminalRuntimeState::default()));
        update_current_directory_from_output(
            &runtime_state,
            "\u{1b}]7;file://server/home/daniel\u{7}".as_bytes(),
        );
        track_potential_directory_change(&runtime_state, b"cd /tmp\r");

        let state = runtime_state.lock().expect("state should lock");
        assert_eq!(state.current_remote_directory, "/tmp");
    }

    fn assert_startup_has_alias_trio(startup: &str, command: &str) {
        assert!(startup.contains(&format!("alias {command}='__kortty_agent_emit execute';")));
        assert!(startup.contains(&format!("alias {command}-ask='__kortty_agent_emit ask';")));
        assert!(startup.contains(&format!("alias {command}-plan='__kortty_agent_emit plan';")));
    }
}
