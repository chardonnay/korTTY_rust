## Using the manual {#manual-reader}

Open **Help → Manual** or press **F1**. The manual is bundled with kortty and opens in one native window. **AI Manager** and the **Guide** button in Settings open their matching chapter in this same window.

### Find a section {#manual-search}

Enter words in **Search the manual** to filter chapters and paragraphs. Choose a chapter in the left column. Its section buttons go directly to a heading. An internal link, such as [SFTP transfers](#sftp-transfer-queue), clears the search and opens the exact section. Only destinations in this bundled manual are active links.

Use **A−**, the percentage button and **A+** to change or reset the saved zoom. **Ctrl/Cmd+F** focuses search; **Ctrl/Cmd+−**, **Ctrl/Cmd++** and **Ctrl/Cmd+0** adjust zoom. Diagrams scale with the text and retain their proportions. Scroll wide diagrams horizontally.

### Ask and review sources {#manual-questions}

**Ask the manual** uses the default text profile from [AI Manager](#ai-manager). Enter a question and choose **Review question**. Review the displayed destination, masked question and selected manual excerpts before choosing **Send reviewed question**. Cancelling stops the request. Changing the AI configuration invalidates the review.

The answer is based only on those excerpts. Its numbered source buttons show the source title and a preview. Select a source to open its exact **Source excerpt**, followed by the surrounding paragraph. References are bound to the reviewed excerpts; text returned by a model cannot create new navigation destinations.

## Connections and routes {#manual-connections}

Use **Connections…** to save and organize SSH targets, serial profiles and local shells. Groups, tags and favorites help find reusable profiles. Review imported entries before saving them. Choose [SFTP](#manual-sftp) for file operations on an SSH target.

### Verify an SSH route {#connection-verification}

A jump host and its destination have separate logins and host key pins. Review the fingerprint for each endpoint before accepting a new key. A changed key stops login until the replacement has been reviewed. Confirm saved tunnel rules for the displayed route before they start.

![SSH route from a saved target through separate host key checks to an authenticated terminal](src/workspace/chrome/manual/connection-review.svg)

### View the network map {#connection-network-map}

The Connection Manager can display a network map of the saved targets, jump routes and tunnel relationships. Use its layout and zoom controls to inspect a route. The map presents the saved configuration; an edge does not establish a connection or approve a host key.

### Terminal preferences {#connection-preferences}

**Show terminal overrides** in a saved SSH or local shell profile lets you choose its theme, font, colors, encoding, close confirmation and default terminal AI chat profile. Unset values inherit the current defaults. **Use global defaults** resets the draft overrides; save the profile to apply the reset.

### Shell integration and Agent shortcuts {#connection-shell-integration}

For a local terminal, enable **Add shell integration automatically** in Quick Connect or the saved local shell profile. Supported shells include Bash, zsh, fish, PowerShell and CMD. Automatic setup applies only to that launch. For SSH, open **Settings → Terminal → Set Up Shell Integration…**, install the matching snippet in the remote user's startup file and reconnect.

Enable **AI Manager → General → Enable terminal agent shortcuts**. At an empty live prompt, type **agent** and press **Tab** to see matching commands. Add a goal and press **Enter** to open the Agent. The terminal must use Xterm with UTF-8 and have shell command marks enabled. Right-click **Set Up Shell Integration…** to see readiness for that exact pane. The built-in Agent can also be opened from **AI → AI Agent…**.

PowerShell keeps the existing prompt. CMD uses a drive/path prompt for the session and supplies prompt boundaries, without command timing or exit-status reports. Supported custom shell profiles can use interactive/login arguments. Automatic integration is unavailable for commands that execute a script or command directly.

## SFTP file manager {#manual-sftp}

Open **SFTP…** from a connected SSH pane to browse local and remote folders using the verified connection. A separate SFTP login is also available. Closing the SFTP view leaves the terminal session open. [SFTP settings](#settings-sftp) control transfer concurrency and idle closing.

### Navigate and inspect folders {#sftp-navigation}

Select a path segment above either pane to open that ancestor, or edit the full path. Filters accept a filename fragment or wildcard. **Calculate folder size** totals ordinary files below the selected directories, skips links and checks the tree again before showing a result. Cancelling a server calculation closes its SFTP channel. Reconnect before continuing to browse that channel.

**Search subfolders…** on the server reviews the starting folder and respects the hidden-file setting. Opening a search result rechecks its path. **Compare folders…** compares the two current folders before offering a reviewed copy of missing entries.

### Transfer queue and conflicts {#sftp-transfer-queue}

Select files or folders to upload or download. The queue shows individual transfers and progress. Pause the queue to stop starting more work; cancel a transfer to stop that operation. Review a destination collision before replacing, skipping or keeping a copy under another name.

![SFTP queue using one verified SSH connection, bounded transfer channels and conflict review](src/workspace/chrome/manual/sftp-transfers.svg)

The parallel limit defaults to 3 and can be set from 1 to 8. A limit of 1 transfers through the browser channel. Higher limits open additional channels on the same verified SSH connection. Lowering the limit lets existing transfers and conflict reviews finish before starting more work. Managed policy can reduce the limit or disable transfers.

### Edit and close {#sftp-edit-close}

Opening a remote file in the native editor keeps its SFTP connection available. Review changes before saving. When closing a view with unfinished work, review the close prompt. Declining the close keeps pending work and any earlier conflict review available.

Optional idle closing is off by default. It waits while transfers are queued or running, a dialog is open, or a remote editor is open. A timeout explicitly saved in a project takes precedence over the global [SFTP timeout](#settings-sftp).
