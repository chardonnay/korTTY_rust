## Settings · Font

Choose a terminal font family and size. The preview shows the draft. **Save** updates panes that use the global size. Panes with an explicit zoom keep their own size.

## Settings · Colors

Set the terminal foreground, background, cursor, selection and palette colors. Use the preview to compare the colors against terminal text before saving.

## Settings · Themes

Choose a saved terminal theme to apply its colors and appearance together. The preview shows how the selected theme renders terminal text.

## Settings · Appearance

Choose the application design and whether to animate design effects. This page controls the application chrome. The terminal color palette is in **Colors** and **Themes**.

## Settings · Terminal

Set the initial columns and rows for local shells and SSH sessions. A blank field uses the terminal emulation's default. Saved connections can override either dimension in the Connection Manager.

Configure the scrollbar, bold text, command marks, command timestamps and scrollback compression. The link and Quick Select controls affect links and matches in terminal output.

Under **Paste protection**, choose when to review pasted text and set the size threshold. The clipboard controls let you grant terminal programs the separate permissions to write text with OSC 52 and request clipboard data with OSC 5522. Managed policy can restrict these controls.

## Settings · Messaging

Set the number of seconds without output that counts as silence for a monitored tab. The tab context menu lets you monitor a tab for silence or for activity.

## Settings · Video

Enable terminal recording after an app restart and choose whether new recordings capture terminal colors. Start or stop a recording explicitly from **Tools → Start/Stop Terminal Recording**. **Tools → Video Manager…** opens the recording library.

## Settings · Backup

Set the automatic backup schedule, destination, retention and encryption options. Use **File → Create Backup…** for a manual backup and **File → Import Backup…** to review and restore one.

## Settings · Logging

Choose a directory and retention period for the application log. Under **Session journal**, choose the storage folder and the capture log format for new journals.

Journal screenshot analysis uses a saved, enabled local model with image input allowed. **Automatically analyze screenshots with AI** applies to new screenshots. Each live recording can also opt out.

## Settings · Export

Set the default watermark and footer for supported journal and snippet analysis exports. The watermark appears on PDF pages and HTML snippet analysis reports. The footer appears in PDF, HTML and Markdown.

**Include full script in analysis reports by default** preselects the script appendix for PDF, HTML and Markdown. The single-report dialog saves changes to this default. A change in the batch dialog applies only to that export.

## Settings · Window

Configure startup geometry, the dashboard, session restoration and toolbar icon size. Under **Tabs**, choose whether supported tools open as tabs and whether the focused split pane or connection gets a frame.

## Settings · Keyboard

Find a command by its name or menu and record the keys you want to use. Resolve duplicate assignments before saving. Reset a command to restore its default shortcut.

## Settings · Security

**Change master password…** opens the Credential Vault. If the vault is locked, unlock it first. The vault then asks for the current password and the new password twice. Enter and confirm them in the vault window.

Enable **Require master password at startup** and choose **Save** to unlock or create the vault before the next workspace opens. An installation policy can require this and lock the setting.

For an existing vault, choose **Configure automatic unlock…**, unlock the vault if needed, and confirm **Enable automatic unlock**. kortty stores an unlock key in the device's protected credential store. The personal or managed startup password requirement takes precedence. If the store is unavailable, enter the master password. Choose **Disable automatic unlock** to remove the device credential. The choice applies to this profile on this device.

Enable **Enable temporary SSH key option** and choose **Save** to use the temporary-key method for SSH or SFTP in Quick Connect. Enter the host and username, set the key lifetime, and use **Paste private key**. The key is held in memory for the attempt and its retries until it expires. For a saved connection with reusable key authentication, use a registered SSH key.

Use **Default isolation** for new local shells and new SSH or SFTP logins. Saved personal SSH connections keep their own setting. Teamwork connections can raise the default, and the managed minimum always applies. The Security page reports the local shell and SSH sandbox backends separately. To start a local sandbox, set an absolute workspace in **Session security**. Existing local-only settings apply until a default is selected in Global Settings.

## Settings · Editor

Choose a block, line or underscore cursor and enter a cursor color as `#RRGGBB`. File editors use the terminal font and colors. Use **Reset zoom** in a file editor to restore the current terminal font size.

## Settings · Snippet Editor

Choose an installed font, a font size from 0 to 72, and foreground and background colors for snippet editors and previews. Use **Inherit terminal font**, enter `0` for the size, or clear a text color to follow the corresponding terminal setting. The cursor style and color can override the corresponding file editor settings independently. Select **Inherit** for the cursor style or clear the cursor color to follow the file editor. Enter colors as `#RRGGBB`. The text preview reflects your draft. **Save** applies the changes to open snippet editors and keeps them for the next launch.

## Settings · SFTP

Set the maximum number of parallel transfers from 1 to 8. The default is 3. A managed policy can lower the limit. Files selected together can transfer on separate channels of the same verified SSH connection. A limit of 1 uses the browser channel for transfers.

Enable **Close SFTP after inactivity** to close idle windows and hosted SFTP tabs after 1 to 120 minutes. A timeout saved in a project takes precedence. The timer waits while transfers are queued or running, a dialog is open, or a remote editor is open.

See [Transfer queue and conflicts](#sftp-transfer-queue) for the channel limit and [Edit and close](#sftp-edit-close) for pending work.

## Settings · AI Agent Design

Choose the default design for newly opened AI agent activity. A saved connection can select its own design. Choose a design to preview it, or use **New design** or **Duplicate selected** to create a custom layout.

Custom designs can change activity history, terminal status, approval controls and activity window arrangement. Rename or remove a custom design in this page. Removing it makes connections that used it follow the default. **Save** applies the draft catalog and default.

**Preview a module** substitutes one role in the demo. **Reset preview** returns to the selected combination. The demo uses the same history, status, and floating presenters as a terminal run. **Replay** starts the sample again. For Adaptive Dock, select a sample terminal size to see its narrow, medium, or wide layout.

Set how long the status stays after a run. The default is six seconds and zero keeps it visible. **View → AI Agent Panel → Show or Hide Status Bar** restores or hides the current pane's status. **Reduce Agent design animations** stops decorative motion in the preview and in terminal Agent designs. **Save** also applies these two preferences.

**View → AI Agent Panel → Ask in Spotlight…** opens a prompt in the current terminal pane. Choose a profile or use the terminal default. Tab switches between the prompt and profile selection. Up and Down browse profiles or recall saved prompts in regular panes. Enter starts the usual Agent review for that pane. Escape closes Spotlight.

## Settings · Updates

Enable **Check automatically for Rust updates** and choose an interval from 1 to 30 days. Automatic checks are off by default. A managed policy can disable update checks.

Available archives are reviewed in **Help → About kortty**. Checks use the stable Rust release channel shown in this page. See the displayed status for the last result. Enabling a scheduled check does not install an update.
