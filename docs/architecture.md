# InputWeave architecture

InputWeave lets the computers on one local network share a keyboard and a mouse: the pointer crosses a screen edge onto the next computer, the clipboard follows it, and files can be sent between them. A background service written in Rust does the work; each platform has a native app that only talks to it.

This document describes how the pieces fit together and the rules they keep. Section numbers are stable, so code comments can point at them (`docs/architecture.md §6`). Details of a single component live in its code and tests.

## 1. Processes

Every Rust role is a subcommand of one executable, `inputweave`, so packaging stays simple and the roles never disagree on a version:

| Role | Runs as | Does |
|---|---|---|
| `agent` | the user (macOS, Linux); SYSTEM in the active console session (Windows) | the core: networking, input capture and injection, clipboard, files |
| `service` | LocalSystem in session 0 (Windows only) | starts and supervises the agent and the broker, calls `SendSAS`; never touches the network |
| `broker` | the logged-on user (Windows only) | what SYSTEM must not do: clipboard, reading and writing the user's files, desktop pictures, the transfer history, launching the app |
| `ctl` | the user | command-line client of the agent's IPC |

```
macOS (all in the user's Aqua session)
  InputWeave.app (SwiftUI: menu bar panel + settings window)
    └ Contents/Library/LoginItems/InputWeave Agent.app   ← LaunchAgent registered with SMAppService.agent
          │ IPC: Unix domain socket + protobuf
     inputweave agent ── core, network, capture/injection, clipboard, files
                         holds the permissions: Accessibility, Input Monitoring, Local Network

Windows
  Session 0:  InputWeave service (inputweave service, LocalSystem)
                 watches sessions, starts and supervises the two below, SendSAS. No network.
                 │ CreateProcessAsUser
  Active console session:
     inputweave agent (SYSTEM token) ── core, network, capture/injection,
                                        follows the input desktop (Default ⇄ Winlogon/secure desktop)
          ▲ named pipe                  ▲ named pipe
     inputweave broker (user token)      InputWeave.exe (WinUI 3, user)
       clipboard, files, wallpaper         notification area icon + settings window

Linux
  inputweave agent as the systemd user service app-cc.ctsite.inputweave@agent.service
  (clipboard and files in the agent itself; no service, no broker)
     ▲ Unix domain socket
  inputweave-app (GTK 4 / libadwaita)
```

## 2. Background service and UI are separate

Why:

1. On Windows only a SYSTEM process can inject into UAC prompts, the lock screen, the sign-in screen and elevated windows (UIPI).
2. On macOS, quitting the menu bar app does not stop sharing, and launchd restarts the agent if it exits.
3. A crashing UI never interrupts sharing, and one agent serves the app and `inputweave ctl` at the same time.

### 2.1 Windows: session 0 and the input desktop

The service in session 0 cannot capture or inject input on the user's desktop, so:

1. The service duplicates its own SYSTEM token and sets `TokenSessionId` to the active console session (`WTSGetActiveConsoleSessionId`).
2. It starts the agent with `CreateProcessAsUser` on `winsta0`, and starts it again on `SERVICE_CONTROL_SESSIONCHANGE` (user switch, sign-out, remote desktop taking over) and when someone signs in or out, so the agent's pipe admits the right user. The agent always runs in the console session, also when nobody is signed in, so it can operate the sign-in screen; the broker runs only while someone is signed in.
3. The agent's capture and injection threads follow the current input desktop (`OpenInputDesktop`, `SetThreadDesktop`), reinstalling their hooks after a desktop switch. Low-level hooks and injection only work on the desktop that has input right now.
4. Ctrl+Alt+Del: when the agent would inject Delete with Ctrl and Alt held, it asks the service over a SYSTEM-only pipe to call `SendSAS(FALSE)`. The installer sets the `SoftwareSASGeneration` policy this needs.
5. Supervision: a child that exits is started again after 250 ms, backing off to 2 s when it keeps exiting. Each child is created suspended and put in a kill-on-close job object only the service holds, so if the service itself dies, its children go with it and the service's failure actions restart everything cleanly.

### 2.2 Windows: what a SYSTEM process open to the network must not do

- No application message is parsed before mutual TLS with a pinned fingerprint has completed. An unpaired connection can only enter the pairing protocol, whose messages are size-limited.
- Every message has a size limit; the protobuf decoders are fuzzed.
- **Clipboard access and file I/O never run as SYSTEM.** The broker does them with the user's token; otherwise a path-traversal bug would become privilege escalation. The agent streams received data to the broker, which writes it as the user. Files to send are opened, hashed and read by the broker too: any process of that user can reach the agent's pipe, so if the agent opened the paths it is given, any program could make it read files the user cannot. The transfer history, which holds the user's file names, is also written by the broker in the user's own profile.
- The agent's named pipes admit only SYSTEM and the session's logon SID: `D:P(A;;GA;;;SY)(A;;GRGW;;;<logon SID>)`, remote clients refused.
- Configuration and the identity key live in `%ProgramData%\InputWeave`, writable only by SYSTEM and Administrators.

On Windows a pairing trusts the whole computer: a paired device can control any user signed in there, and the sign-in screen.

### 2.3 macOS: permissions belong to the agent

- The agent is a helper app bundle inside the main app with its own bundle id; launchd starts it, so it is the responsible process that macOS grants Accessibility, Input Monitoring and Local Network to. System Settings lists it by its bundle name, "InputWeave Agent".
- **The SwiftUI app never calls an API that triggers a permission check** (Accessibility, CGEvent, `CGRequestListenEventAccess`). The agent detects its own state with preflight calls (`AXIsProcessTrusted`, `CGPreflightListenEventAccess`, `CGPreflightPostEventAccess`) and reports it over IPC; the app explains and opens the right pane of System Settings.
- A process keeps the permission state it first read, so while a permission is missing the agent asks a child process (`inputweave permissions`) once a second and re-executes itself when it is granted. Long-running processes must not send non-preflight requests to tccd.
- The signing identity and bundle id must stay the same across releases, or every update loses the permissions.
- The lock screen and the login window hold Secure Event Input: WindowServer drops synthetic events from `CGEventPost`/`IOHIDPostEvent` there. The only way in is input that looks like hardware: a virtual HID device (`IOHIDUserDeviceCreateWithProperties`), which needs Apple's managed entitlement `com.apple.developer.hid.virtual.device`. Without it the agent falls back to `CGEventPost`. While FileVault's unlock screen is up, macOS has not started and no agent exists.

## 3. IPC

- **Transports:**
  - macOS: Unix domain socket `~/Library/Application Support/InputWeave/agent.sock` (0600); the agent checks the peer's uid with `LOCAL_PEERCRED`.
  - Windows: named pipe `\\.\pipe\InputWeave\agent-<SessionId>` with the DACL of §2.2. The broker uses its own pipe, `\\.\pipe\InputWeave\broker-<SessionId>`, and the `inputweave.service.v1` messages.
  - Linux: `$XDG_RUNTIME_DIR/inputweave/agent.sock` (0600, peer uid checked).
- **Format:** a u32 length prefix and a protobuf message (`iw_proto::frame`, with a size limit). Clients send `ClientMessage { request_id, oneof command }`; the agent answers `AgentMessage { oneof { Response, Event } }`. The two sides exchange an IPC version first; an incompatible pair usually means an update is under way, and the app says so while it waits for the new agent.
- Several clients may be connected (the app, `ctl`); status events go to all of them.
- The apps implement the client themselves (swift-protobuf, Google.Protobuf, prost) and never link the Rust core.
- **The UI is the only thing that speaks to the user.** When a decision is needed (a pairing code to show, a file to accept) and no app is connected, the agent first launches the app, at most once every 10 seconds: on macOS with `open -g -b cc.ctsite.inputweave`, on Windows through the broker (the SYSTEM agent starts no user processes), on Linux by D-Bus activation of `cc.ctsite.inputweave`. The agent and the broker never show dialogs of their own.
- New features are new `Command` and `Event` messages; the UI holds no business logic.

## 4. Crates and repository layout

Dependency direction: `iw-keymap` ← `iw-core` ← `iw-platform`; `inputweave` ties them together.

| Crate | Responsibility |
|---|---|
| `iw-proto` | Code generated from `proto/` by protox (no `protoc` needed), and the frame format shared by IPC and peer streams. Packages: `inputweave.peer.v1` (between devices), `inputweave.app.v1` (app IPC), `inputweave.service.v1` (agent, service and broker on Windows) |
| `iw-core` | **sans-IO** state machines: layout and edge crossing, routing and the virtual cursor, safety (heartbeat, release-all), clipboard and file coordination. Events in, `Action`s out; it holds no sockets, threads, timers or OS handles, and the caller passes the time in. Tests need no mocks |
| `iw-net` | QUIC (quinn), self-signed certificates, pairing (SPAKE2), mDNS/DNS-SD. Which devices to trust is the caller's decision |
| `iw-ipc` | IPC transport: Unix domain sockets on macOS and Linux, named pipes on Windows, with the same public types |
| `iw-keymap` | USB HID usages ⇄ macOS virtual key codes, Windows scan codes, Linux evdev codes, generated from Chromium's `dom_code_data.inc` (`cargo xtask keymap`) |
| `iw-platform` | Traits for platform features (`InputCapture`, `InputEmulation`, `Displays`, `DesktopPictures`, clipboard…) with an implementation per `cfg(target_os)`, and a `mock` used by the agent's integration tests. Capture sends events over `std::sync::mpsc`, so platform threads with run loops need no async runtime |
| `iw-winsvc` | Windows only: the service, session tracking, tokens and `CreateProcessAsUser`, supervision, `SendSAS`, the broker. Elsewhere only its FFI-free decision logic and the broker protocol compile, for tests |
| `inputweave` | The one binary, dispatching on its role |

Rules for the code:

- Platform APIs appear only in `iw-platform`, `iw-winsvc`, and the named-pipe transport of `iw-ipc`.
- The FFI-free parts of a platform backend (coordinate conversion, event translation, state machines, string building) live in modules not restricted by `cfg(windows)` or `cfg(target_os = "linux")`, so their unit tests run on every host; the `unsafe` FFI layer is kept thin.
- Dependencies are permissive licenses only (`deny.toml`); the project's own license is GPL-3.0-or-later.

Outside Rust: `proto/` (the single schema for Rust, Swift and C#), `apps/macos` (Xcode project generated by XcodeGen, SwiftUI, the agent helper bundle), `apps/windows` (.NET 9, WinUI 3), `apps/linux` (its own cargo workspace, GTK 4), `packaging/windows` (WiX MSI), `packaging/linux` (`.deb`), `xtask` (code generation).

## 5. Network and security

### 5.1 Discovery

mDNS/DNS-SD, service type `_inputweave._udp.local` (the `mdns-sd` crate), with typing an IP address as the fallback. TXT records: `id`, `name`, `os`, `fp` (the full certificate fingerprint), `pv` (protocol version range), `pair` (accepting new pairings). Discovery is never trusted: it only supplies names and addresses. The dialing side tries the configured `address`, then the address mDNS saw, then the last address it connected to.

### 5.2 Transport

One QUIC connection (quinn, TLS 1.3) between each pair of devices, split by the kind of data:

- **Pointer motion and scrolling:** QUIC datagrams (RFC 9221) with sequence numbers; only the newest matters, a lost one does no harm.
- **Keys and mouse buttons:** a reliable, ordered input stream. Button events carry absolute coordinates, so the receiver moves there first and the order between datagrams and the stream cannot misplace a click. `Enter` and `Leave` go on the same stream so they stay ordered with the keys between them.
- **Clipboard and files:** streams of their own, so they never block input.
- **Control** (handshake, heartbeat, layout sync): the control stream.
- **Priorities** (`iw_net::priority`): input and control 1, clipboard 0, files -1; quinn sends a lower priority only when higher ones have nothing to send, and datagrams go before stream data in every packet. Queues further along the path are handled by file pacing (§9).
- **Dial direction:** only the device with the smaller device id dials; the other refuses a connection the other way, so each pair has exactly one session.
- **ALPN:** `inputweave/1` for sessions, `inputweave-pair/1` for pairing. A session checks the peer's fingerprint against the trusted list before reading any stream.
- **Liveness:** an application heartbeat every 250 ms with a 1 s timeout; QUIC keep-alive (500 ms) and idle timeout (3 s) behind it.

### 5.3 Identity and pairing

- Each device has a long-term key and a self-signed certificate. Its device id, unless configured, is the hex of the first 8 bytes of the certificate fingerprint; its name defaults to the host name.
- Pairing: the device being added shows a 6-digit code; the user types it on the device that started the pairing; SPAKE2 (Ed25519 group) with the code as password confirms both sides, binding each side's certificate fingerprint and device id as seen in TLS, so a man in the middle makes the two sides derive different keys. The keys are only used for HMAC-SHA256 confirmation, initiator first, so a wrong guess yields nothing to check offline.
- A code allows 3 attempts within 2 minutes; after any wrong guess the device answers "busy" for 30 seconds, whatever happens to that pairing.
- The device being added only needs to be seen, not operated: it may have no keyboard or mouse yet.
- On success both sides store the other's fingerprint, device id, name, OS and address in `devices.toml` (0600, written atomically). A configuration file's `[[peers]]` (fingerprints printed by `inputweave identity`) is the other source of trust. Pairing a Mac with a non-Mac turns on the ⌘ ⇄ Ctrl swap by default.
- Later connections are mutual TLS with the pinned fingerprint (a custom rustls verifier).

### 5.4 Protocol versions

The handshake exchanges a minimum and maximum protocol version; an incompatible pair is reported, never downgraded. Within a `v1` protobuf package only backward-compatible additions are allowed; newer behavior is gated on the peer's protocol version.

### 5.5 Firewalls

The Windows MSI adds firewall rules. The macOS agent declares `NSLocalNetworkUsageDescription` and `NSBonjourServices`. The Linux package ships a ufw application profile and a firewalld service but does not change the firewall itself; the app explains the one command needed when ufw blocks InputWeave.

### 5.6 Waking a device

A device can wake a sleeping paired one over the network (Wake-on-LAN):

- **Telling:** once a session starts, each side tells the other how to wake it (`WakeInfo`, on the control stream): the hardware address of the network interface that session runs on, and the broadcast address of that interface's IPv4 network. The session finds the interface by the address its end of the connection uses, a link-local one by its scope, saying nothing when it can't tell the interface (`iw_platform::network`: `getifaddrs` with `AF_LINK` on macOS and `AF_PACKET` on Linux, `GetAdaptersAddresses` on Windows), looks again every minute, and speaks only when it changed. An interface without a hardware address (a VPN, a tunnel, the loopback) says nothing. Older peers ignore the message, and can't be woken.
- **Keeping:** the receiver checks it (6 bytes, not zero nor a group address; a broadcast address below 224.0.0.0 outside 0/8 and 127/8), takes at most one every few seconds from a session, and keeps it by device id, with that device's fingerprint (another device trusted under the same id later gets none of it), in `wake.toml`, a file of its own beside `devices.toml` (so an older agent, which reads only the devices file, still starts), with when that device was last heard from, so it can be woken while offline and the apps can say since when it is. That time is the wall clock's at the heartbeat when the device was last heard (never one worked out from a monotonic clock, which stands still while a Mac sleeps), written as its session ends, every ten minutes while it is connected (so a crash or a stop without notice leaves it that late at worst) and as the agent stops; a session replaced by another writes nothing. It changes without publishing the trusted devices anew. Unpairing forgets it; pairing again under another id takes it along.
- **Waking:** a magic packet (six `0xFF` bytes, then the hardware address 16 times) by UDP to port 9 of that broadcast address, only while it lies on one of this device's own IPv4 networks at the time (the address is the other device's word), and of 255.255.255.255, from the agent (the SYSTEM agent on Windows, which owns the networking), off the engine's loop. The device then counts as being woken for two minutes, or until it connects; one that didn't connect in time is reported as not woken until it connects or is woken again. Neither this device nor a connected one is ever woken.
- **Pushing toward it:** with that option on, the router (sans-IO, like the rest of `iw-core`) asks for an offline device that can be woken when the cursor pushes against an edge toward it, where it would cross were that device connected: after the wait at the edge and with the switch modifier, as crossing needs, never in relative mode, and at most every 30 seconds for each device. The engine checks that last against every wake-up it sent, so a push seconds after Wake was chosen in an app sends none; one asked for from an app or the command line always goes. On a Wayland desktop the cursor reaches the agent only at edges toward connected devices, so pushing there wakes nothing.

## 6. Input sharing

### 6.1 Symmetric peers

Every computer can be the controller: whichever one the physical keyboard and mouse are being used on. The controller owns the group's layout and a **virtual cursor**; it sends absolute positions in the target's logical coordinates and switches targets across several hops itself.

### 6.2 Layout

- The group shares one versioned layout document (`Layout`: each computer's desktop origin on the canvas). Versions order by version number, then author device id; a device that receives a newer one replaces its own whole and forwards it, so connected devices converge. Whole replacement rather than per-device merging keeps the result internally consistent when two devices place each other at once.
- Users move **whole computers** on the canvas; the screens within one computer keep the arrangement its OS has.
- The canvas uses **logical units** (pixels ÷ scale); conversion happens only at the `iw-platform` boundary. The Windows agent is PerMonitorV2-aware.
- A device not yet arranged is placed when its displays first arrive (the pairing initiator puts the other to its right, the other side to its left, main screens' top edges aligned).
- The layout and every device's last displays are kept in `devices.toml`, so offline devices still appear. A configuration file's `[[layout]]` applies locally only and is not shared.
- Desktop picture thumbnails (small JPEGs, size-limited on both sides) travel in the `Displays` message so the canvas can show each screen's wallpaper; a privacy switch stops sending them.

### 6.3 Crossing an edge

`Layout::step`: moving between a computer's own displays works as usual. When the pointer leaves a display through one side:

- The candidates are all displays beyond that side, this computer's own included; devices that cannot be entered (offline, locked, a mouse button held — except while dragging files, §6.4) are left out.
- The nearest edge facing that side wins (Euclidean distance from the exit point). If it belongs to another computer, the pointer enters it at the nearest point of that edge plus the remaining motion, so gaps between computers are skipped. If it is this computer's own display, the pointer stops: it never passes over its own screens to a farther computer.
- The corner exclusion zone applies to the display being left only. Where another computer is beyond a side (and no own screen is closer), there is no wall; computers need not touch exactly on the canvas.
- Where displays of different computers touch, the shared segments are **seams** (`Layout::seams`); the apps draw them, and the pointer crosses them straight through.

### 6.4 Routing

1. When the pointer crosses to another computer, the controller grabs local input and hides and holds its own cursor (macOS: an active event tap swallows events; Windows: the low-level hooks return non-zero; Linux: see §7).
2. It reads relative motion, updates the virtual cursor and sends the absolute position in the target's logical coordinates.
3. Entering yet another computer switches the target directly; returning to the controller's own screens releases the grab.
4. **Lock** (a shortcut, or IPC `SetLocked`): edges are not crossed while locked; screens of the same computer still are. Not saved across agent restarts.
5. **Jump to a device** (IPC `JumpTo`, jump shortcuts 1–9): the virtual cursor goes to the centre of that device's main screen, as if it had moved there.
6. **Shake to locate:** `iw-core::shake` recognises a shake of the mouse (4 direction changes within 1 s); the device the pointer is on asks its app to highlight the cursor. It does not affect routing.
7. **Dragging files across computers:** normally a held button blocks crossing (a drag cannot be split across two computers). When an app reports a file drag near a seam (`BeginDrag`), the router lets the held button cross; the source's drag ends in the app's drop zone and the target's app sees a drag in progress. The source may also be a computer being controlled; its agent tells the controller with peer `RemoteDrag`.
8. **Shortcuts** (`iw-core::hotkey`): take back control, lock, relative mouse and jump; per computer, the controller's own apply. Lock and jump shortcuts are reserved in the capture layer (never delivered locally; on X11 each is grabbed on its own, so one another client holds reaches local apps while the others stay held back). Capture tells the router which are held back (`InputCapture::reserve_hotkeys`, then each change as it comes, in order with the keys and numbered, so a set told earlier never replaces a later one: `InputEvent::HotkeysHeldBack`; X11 holds none back while another client's grab takes every key, and grabs one another client holds again, less and less often), and the router remembers, as a shortcut's key goes down, whether local apps saw it, from that set alone: such a key still down when input is grabbed is released locally, one held back is not; take back and relative mouse matter only while grabbing, when no input reaches local apps anyway. Take back and lock may also be a single key typing never uses (Scroll Lock, Pause, F13–F19); the agent refuses any shortcut on a key its platform never reports (`Hotkeys::check_keyboard`, `iw_platform::keyboard_reports`: a Mac has no Scroll Lock or Pause). A key alone fires whatever modifiers are held, so it clashes with every shortcut on the same key that is looked at after it. Taking control back can also be Control pressed twice (`Hotkeys::take_back_double`, IPC `Hotkeys.take_back_double`, off by default): the router sees the same Control key pressed and released twice within 400 ms of the first press, with no other key, button or scroll in between, while it controls another computer and not in relative mouse, where games often use Ctrl, using the times the caller passes in. Unlike a shortcut, those Control presses are forwarded as usual; the second release goes out before the `Leave`, which releases everything on the target anyway. The rule is `iw_core::hotkey::DoublePressWatch`, which the router uses and so does X11 capture while it waits for a grab another app holds, timed by the X server's own event times (counted anew when it connects again or a time goes far back, as a restarted server's does): then nothing but the shortcuts goes out as keys, and a completed double press is reported as such (`InputEvent::DoublePressed`), which takes control back without sending the target anything: those Control presses went to the app holding the grab. `SetHotkeys` is applied by the engine, which fills in what an older app leaves out (`relative`, `take_back_double`) from the shortcuts in effect at that moment (what a client sends replaces the rest, the last one winning), and is answered once the status shows the result and the devices file has it (rewritten whenever what it holds differs, even when the shortcuts in effect do not change, as when it held unusable ones); the engine has the runtime write the file on a blocking thread, which finishes whether or not the client waits, a later change's write winning over an earlier one's. The answer waits a couple of seconds at most (`timeout`, the change going on), and says when the file could not be written (`not_saved`; the change stays in effect, forgotten when the agent stops unless a later change is saved). A shortcut the devices file holds on a key this keyboard never reports, as one set on a PC on Scroll Lock or Pause read on a Mac, is replaced alone, the others kept, by the first default that clashes with none of them (its own, then the other shortcuts' defaults; relative mouse off last; `Hotkeys::reported_only`).
9. **Relative mouse** (for games that hide the cursor and re-centre it every frame): on request, by shortcut, or automatically when the target reports that an app captured the pointer, the controller sends motion deltas instead of positions and crosses no edges. Sessions are numbered so late datagrams of an earlier session never apply; when it ends, the target reports where its cursor is and the controller continues from there.
10. **Pause sharing:** the computer neither controls nor is controlled; peers are told and treat it as unreachable.
11. **Cursor speed** (IPC `SetDeviceSpeed`, `DeviceStatus.speed_percent`): the controller's user may make the cursor move faster or slower on another trusted computer, 50% to 300% in steps of 10. Each part of an ordinary motion is scaled by the speed of the computer that part is on (`Layout::step_at`): a motion that crosses an edge goes at the speed of the computer it starts on up to the crossing and at the speed of the computer it enters after it, so crossings still happen where the seams are. On the controller itself the cursor always moves as the mouse does: leaving it, the part up to the edge is unscaled and the rest goes at the entered computer's speed; coming back, the part past the edge is unscaled again. Relative mouse deltas are never scaled. It is the controller's own preference, kept in `devices.toml` by device id (like the names given to other computers) until the computer is unpaired, and never sent to other computers. `SetDeviceSpeed`, like the other commands that change a trusted device (`SetDeviceSharing`, `SetDeviceOptions`, `RenameDevice`), is answered once the status reflects the trusted devices as of the change (a generation count), not once it shows the value asked for, which a later change may already have replaced.

### 6.5 Avoiding accidental switches

- A corner margin keeps hot corners working.
- Optional dwell: the pointer must push against an edge for a while before it crosses. The core has no timers; the caller passes the time with every event.
- Optional modifier: the pointer crosses only while a chosen modifier is held.

### 6.6 Local takeover

A controlled computer that sees real local input (not injected: `LLMHF_INJECTED` on Windows, our own event source user data on macOS) tells the controller to give control back.

### 6.7 Key codes

Keys travel as physical keys (USB HID usages) and are interpreted by the receiver's own keyboard layout and input method. The ⌘ ⇄ Ctrl swap is each peer's own setting, applied in `iw-keymap`.

### 6.8 Safety: no stuck keys, no lost control

- Heartbeats every 250 ms; after about 1 s without one the controller releases its grab and the receiver releases every key and button it pressed (**release-all**).
- Disconnection, the agent exiting and sleep trigger release-all too.
- The emergency shortcut is handled in the capture layer and never forwarded.

## 7. Platform backends

| | macOS (agent as the user) | Windows (agent as SYSTEM in the user's session) | Linux (agent as the user) |
|---|---|---|---|
| Capture | `CGEventTap` (active tap) | `WH_MOUSE_LL` / `WH_KEYBOARD_LL` on a thread per input desktop | X11: XInput 2; GNOME/KDE Wayland: InputCapture portal + libei; wlroots: layer-shell surfaces at the edges |
| Injection | `CGEventPost` | `SendInput` (absolute, virtual desk; scan codes) | X11: XTest; GNOME/KDE Wayland: RemoteDesktop portal + libei; wlroots: virtual pointer/keyboard |
| Displays | `CGGetActiveDisplayList`, points, scale | `EnumDisplayMonitors` + `GetDpiForMonitor`, PerMonitorV2 | XRandR; `wl_output` |
| Clipboard | `NSPasteboard` (polling `changeCount`), agent | `AddClipboardFormatListener`, **broker** | X11 `CLIPBOARD` selection; Wayland data-control (ext/wlr), else the Clipboard portal; agent |
| Desktop pictures | `NSWorkspace.desktopImageURL(for:)`, agent | `IDesktopWallpaper` + WIC, **broker** | GNOME settings, KDE's configuration, other X11 desktops' root window picture (`_XROOTPMAP_ID`), agent |
| Writing files | agent | **broker** | agent |
| Permissions | Accessibility, Input Monitoring, Local Network (all the agent's) | one UAC prompt at install | portal dialogs (RemoteDesktop remembers with a restore token) |
| Lock / sign-in screen | needs the virtual HID entitlement (§2.3) | yes, through the service and input desktop switching | no |

Notes:

- **macOS:** Secure Event Input on the controller (a focused password field) keeps key events out of the tap; it is detected and the app says so. Hiding the cursor in the background uses a private CoreGraphics connection property, isolated in `iw-platform/src/macos/private.rs`. Scrolling travels in the wheel's physical direction; capture undoes natural scrolling and injection applies the receiver's setting.
- **Windows:** mixed-DPI layouts are converted to logical coordinates by placing each monitor next to one it shares an edge with in pixel space (`iw-platform/src/windows/monitors.rs`), pushing apart overlaps so the mapping stays invertible. While grabbing, the cursor is parked visibly at the centre of its screen and the hooks swallow motion (an invisible system cursor would stay invisible if the agent crashed). Windows silently removes low-level hooks that are too slow, so the hooks only translate events and pass them on.
- **Linux:** the backend is chosen per session (`iw-platform::linux`): X11 uses XTest, XInput 2, RandR and the `CLIPBOARD` selection; GNOME and KDE on Wayland go through the RemoteDesktop and InputCapture portals with libei; wlroots compositors that offer virtual pointer/keyboard and layer shell to clients (sway, Hyprland, river) are used directly, without a portal. The `InputCapture` trait has hooks the Wayland backends need (all default to the behaviour of the other platforms): `wants_exits`/`set_exits` (the edge segments leading to other computers, where Wayland places pointer barriers), `grab_ready`, `edge_wait`, `capturing`, `can_warp`, and `releases_carried_input_locally`.

## 8. Clipboard

- Formats: plain text, RTF and HTML, images (PNG; TIFF-only images are converted).
- **When it moves:** the controller sees every transition, so when the pointer leaves a computer it asks that computer for its clipboard (if it changed) and passes it to the computer being entered, relaying across hops. Content goes on its own transfer stream in chunks, never blocking input.
- Each device remembers the change count it last sent or wrote, so nothing is sent back and forth without a new copy.
- Up to 8 MB moves whole at the transition. Larger content, up to 256 MB (64 MB through the Windows broker), is announced instead (`ClipboardOffer`) and pulled only when an app pastes: the receiver places a promise on its clipboard, and the source answers only for offers it made and only while its clipboard is unchanged. Pulls are limited per device and in total.
- On Windows the broker owns the clipboard; the agent reads ahead on every change notification so the engine never waits on the pipe.
- Clipboard sync can be turned off for the whole computer or per device; blocking happens on this side, the other device need not know.
- **Sensitive content is skipped:** `org.nspasteboard.ConcealedType` / `TransientType` (macOS), `ExcludeClipboardContentFromMonitorProcessing` and the clipboard-history opt-outs (Windows), `x-kde-passwordManagerHint` set to `secret` (Linux). On Windows the broker decides, so sensitive content never reaches the agent.

## 9. File transfer

- Files are sent explicitly: dropped onto a device in the panel or the main window, from a menu, the file manager (Finder's Services menu, which the macOS app provides itself since a Share extension would be sandboxed away from the agent's socket; the Windows "Send to" menu; Dolphin's service menu, `inputweave-app --send`), or by dragging across a seam (§6.4). The apps decide which computers can take them from `DeviceStatus` alone (connected, files not turned off) and list the others with why.
- One send is a **batch** of **items** (files or folders). Folders are listed recursively without following links; every file is its own transfer on its own QUIC stream, chunked, resumable after a disconnection, and verified with BLAKE3. Limits per batch: 1,000 items, 10,000 files, 100 GiB.
- Received paths are checked on both sides (no absolute paths, no empty, `.` or `..` parts, bounded depth and length) and each part is made a valid file name on the receiver. A folder whose name is taken is renamed as a whole ("name (1)"); a file is written as a hidden `.part` file and moved into place only after verification. Failures are per file and never abort the rest of the batch.
- Received files go to `~/Downloads/InputWeave` by default. Files from paired devices are accepted automatically (pairing already grants full control); "Ask every time" is a setting, decided once per batch, refused after 5 minutes without an answer.
- **Pacing** (`agent/throttle.rs`): priorities only decide what leaves this computer first; file data can still fill queues along the path and delay input behind it. Per target, the agent samples QUIC's smoothed RTT at every heartbeat, learns an idle baseline, halves the file rate when the RTT while sending rises more than 15 ms above it (not below 1 MiB/s, at most every 500 ms), and raises it by 3 % per sample once the RTT is back within half that margin. A token bucket spaces the chunks.
- Each side keeps a transfer history; on Windows it belongs to the signed-in user and the broker writes it.
- On Windows the broker writes received files and opens, hashes and reads files to send (§2.2); the agent only gets sizes, hashes and the bytes it asks for.

## 10. Front ends

The apps render `Event`s and send `Command`s; they hold no business logic. UI strings are written in English with a Traditional Chinese (zh-TW) translation. Branding is limited to five color tokens and the seam; everything else uses the platform's semantic colors, fonts and controls.

- **macOS** (`apps/macos`): a `MenuBarExtra(.window)` panel and a settings window; HUDs are non-activating `NSPanel`s. `SMAppService.mainApp` opens the app at login and `SMAppService.agent` registers the agent. Updates use Sparkle with the `.pkg` installer as the update. A new app restarts the agent once when the agent's IPC version, version or build (the SHA-256 of the signed agent executable, `AgentHello.agent_digest`) differs from the one it carries; the app's `MARKETING_VERSION` must equal the Cargo workspace version (checked at build time).
- **Windows** (`apps/windows`): C#, .NET 9, WinUI 3 with H.NotifyIcon, unpackaged and self-contained (no runtime to install); the logic that does not touch the UI lives in `InputWeave.Core` with unit tests. The MSI installs the service, the agent and the app; the app writes its own Run key for "Open at login". Quitting the app leaves sharing running, since the service supervises the agent.
- **Linux** (`apps/linux`): a GTK 4 / libadwaita app in Rust, its own cargo workspace depending only on `iw-proto` and `iw-keymap`, so the root workspace builds where GTK is not installed.

## 11. License

Every feature works without a license and without a time limit. A license, sold through Polar, shows the computer as licensed in the apps' settings and About; one license covers up to five computers, and it reaches paired computers by itself. The code is GPL-3.0-or-later, so anyone may build it without the license check; the license pays for the official builds.

**Who keeps what.** The agent keeps this computer's license record in `license.toml` next to the devices file, readable only by its owner and replaced in one step: the key, its state, this computer's Polar activation id, the buyer's email, when Polar last confirmed it, where the key came from, and whether the user removed the license here. The apps talk to Polar and report each answer to the agent (`RecordLicenseResult`); the agent never connects to the internet, so the network surface of the Windows agent, which runs as SYSTEM, stays the mTLS QUIC sessions. The record is the agent's because the agent is what takes keys from paired devices, so removing a license has to stop that there, and because on Windows the agent is one per computer while an app runs once per signed-in user, so all of a PC's users share one seat. On macOS and Linux the agent, and so `license.toml`, belong to one user: each user account on a computer that activates the license takes a seat of its own.

**States** (`Status.license`): none; waiting to be activated (a key entered here, or taken from a paired device); licensed; seats full; invalid (unknown, revoked or refunded key, or this computer's activation is gone); unavailable (`license.toml` is there but cannot be read right now). Only "licensed" shows as licensed; the others show "Trial" and every feature still works. Unavailable shows "Trial" with "License information can't be read right now." and nothing to change.

**Passing the key on.** After mTLS on a session with a paired device, at protocol version 6 or later, each side sends `LicenseInfo`: its key (empty when it has none, or Polar refused it as invalid) and whether it is licensed with it, and sends it again whenever either changes. A key activated here is never replaced. A computer takes a paired device's key only when it has none, or its own was refused (seats full, invalid), and that device says it is licensed with a different key that Polar has not refused here since a key was last entered here (the agent remembers up to eight refused keys, so two full keys are never tried in turn); it never does after its user removed the license here, until a key is entered here again. Each computer activates the key it took itself, taking one of the license's seats. The apps list the paired devices on the same key as each last said; unpairing removes a device from the list. Nothing is sent while pairing.

**Asking Polar.** The apps call Polar's public customer-portal endpoints, which need no token, at `https://api.polar.sh/v1/customer-portal/license-keys/` (`activate`, `validate`, `deactivate`):

- Every request sends `User-Agent: InputWeave/<version>`. Cloudflare, in front of Polar, answers some default user agents with a 403 of its own; only a 403 whose JSON body has `"error": "NotPermitted"` is Polar's refusal, any other 403 is a network problem to retry later.
- The organization id is `95b9d4a7-5dad-4df5-b309-2cd3383420ae`.
- `activate` sends the key, the organization id and `label`, this computer's name, and no `conditions`; a 200 gives the activation `id` and `license_key.customer.email`. A 403 `NotPermitted` means every seat is taken or the key is no longer active; a `validate` without `activation_id` tells which (200: seats full; 404: invalid). An answer lost on the way (a timeout) may still have taken a seat, so the apps wait longer for `activate` than for the other calls and never ask again at once; a seat taken twice that way is freed in Polar's customer portal.
- `validate` sends the key, the organization id and the activation id, at most every 30 days while the license page or About is open. A 200 confirms; the state becomes invalid only after two 404s at least a minute apart, and an unreachable Polar changes nothing.
- `deactivate` (Remove License) sends the key, the organization id and the activation id, once the agent has removed the license (the app keeps the activation id until then); 204, or a 404 because it is already gone, is done. When Polar cannot be reached, the license is removed here anyway and the user is told to free the seat in Polar's customer portal.
- Polar allows 3 unauthenticated requests a second; a 429 carries `Retry-After`.

**Reporting to the agent** (`RecordLicenseResult`). Keys are 20 to 96 printable ASCII characters without spaces. An answer counts only for the state it answers, and the app says which activation it is about:

- `validated_at` is when Polar answered (the response's `Date` header, or the app's clock where that is more than a day ahead of it): from 2020 to a day ahead of this computer's clock, or the result is `invalid_license_result`.
- After `activate` answered 200: `activated` with the new activation id and the buyer's email. It counts only while the license waits for activation. While this computer is already licensed with another activation (a second user's app, or a late answer), the agent answers `already_licensed`; the same activation reported again (an answer retried) is accepted and changes nothing. When the agent answers an `activated` result with an error code, the app frees that activation with `deactivate`, so it takes no seat, and leaves that key alone for that record (until the user enters it again or the record changes): any code it answers with, known or not (`license_changed`, `license_stale`, `invalid_license_result`, `already_licensed` with another activation, `license_unsupported`, codes from a newer agent), except `license_unavailable` and `agent_stopping`, which are temporary. A failure to get an answer at all (an IPC error; no, an empty or an unreadable answer) is temporary too. A temporary refusal or failure keeps the activation, and the report is sent again after a backoff.
- After `activate` was refused: `refused` (seats full or invalid) with no activation id. It counts only while the license waits for activation.
- After `validate` answered 200: `validated` with `activation_id` set to the activation it asked about. It counts only while licensed with that activation.
- After `validate` answered 404 twice, a minute apart: `refused` invalid with that `activation_id`, under the same condition.
- Anything else is `license_stale` and changes nothing; `license_changed` means the key changed while the app was asking. The app drops such an answer.
- Try Again, after seats full or invalid, sends `SetLicenseKey` with the same key, which starts it over as waiting for activation and forgets the keys refused here. Entering the key this computer is already licensed with keeps its activation.

The agent writes `license.toml` on a thread of its own, only when something kept there changed; a write that fails is tried again, from one second up to every minute, with the newest record, until it succeeds. A file it cannot read as a license is moved aside to `license.toml.bad`, and the agent starts without a license rather than not at all; a `[[peers]]` entry missing a field, or a `validated_at` out of the range above, is dropped alone. A file it cannot read at all (permissions, an I/O error) makes the license unavailable: the agent takes no key from paired devices and tells them nothing (they keep what they last heard), refuses every license command with `license_unavailable`, and never writes the file. It reads the file again every two minutes, off the engine's task; once it can, the license is what the file holds, paired devices are told, and what they said meanwhile is taken in. A state it does not know (from a newer agent) reads as licensed when an activation id goes with it, so the app's next validate settles it, and otherwise as waiting for activation.

`testdata/polar/` holds Polar's response bodies, which the three apps' tests share, and `scripts/mock-polar.py` serves them on 127.0.0.1 for trying every answer by hand. `inputweave ctl license` shows the record with the key masked (never more than its last four characters); `ctl license set`, which reads the key from standard input (not shown as it is typed at a terminal), and `ctl license remove` change it.

## 12. Updates and releases

Every version is published as a GitHub release `v<version>` of `tasict/InputWeave` by `scripts/release.sh`. The apps, the feeds and the Linux package source never name GitHub: they use the website's stable addresses, and the website (`site/_redirects`, on Cloudflare Pages) answers each with a 302 to where the releases are hosted, so the host can change by deploying the site, without a new app:

| Address | Leads to (now) |
|---|---|
| `https://inputweave.ctsite.cc/latest/<file>` | `https://github.com/tasict/InputWeave/releases/latest/download/<file>`, which GitHub redirects to that file of the release marked latest, with no API rate limit |
| `https://inputweave.ctsite.cc/download/v<v>/<file>` | `https://github.com/tasict/InputWeave/releases/download/v<v>/<file>` |
| `https://inputweave.ctsite.cc/releases/v<v>` | the release's page on GitHub |
| `https://inputweave.ctsite.cc/user-guide`, `/zh-Hant/user-guide` | the user guide in the repository, which release notes link to |
| `https://inputweave.ctsite.cc/issues` | the repository's issues |

What an update installs is checked against InputWeave's own keys (12.2–12.4), not against the host it came from. The release script checks, before creating a release, that the deployed site leads these addresses to the repository it publishes to, and afterwards that the site serves the new release's feeds. The release is created as a draft, every file is uploaded and checked, and only then is it published, so `latest` moves once and never to a half-uploaded release. It is marked latest only when no published release has a higher version, so publishing a fix for an older line never moves `latest` back; the script checks afterwards that `latest` leads to the highest version.

### 12.1 The release's files

The names are a contract: the feeds, the package repository and the website's download links use them.

| File | What |
|---|---|
| `InputWeave-<v>.pkg` | macOS installer, universal, signed with a Developer ID and notarized |
| `InputWeave-<v>-x64.msi` | Windows installer (no Authenticode signature) |
| `inputweave_<v>_amd64.deb`, `inputweave_<v>_arm64.deb` | Linux packages |
| `inputweave_<previous>_amd64.deb`, `inputweave_<previous>_arm64.deb` | the previous release's Linux packages, carried over (12.4) |
| `appcast.xml` | Sparkle's feed for macOS |
| `update-windows.json` | the Windows app's feed (12.3) |
| other installers, each with a feed of its own | given to the release script with `--extra-installer FILE:FEED`: published as they are, signed with the same key as the MSI, and described by `FEED` in `update-windows.json`'s form (12.3), its `url` under `/download/v<v>/`; the website's download page does not list them |
| `InRelease`, `Release`, `Release.gpg`, `Packages`, `Packages.gz` | the APT flat repository (12.4) |
| `SHA256SUMS` | the SHA-256 of every other file |

Signatures and checksums are made over the final bytes of each file, after notarizing and stapling. The `.deb` files' package source and the macOS app's feed must name the site's addresses; the script checks both. The Mac that publishes cannot read an MSI, so `apps/windows/scripts/package.ps1` writes `InputWeave-<v>-x64.msi.json` beside it (the ProductVersion the MSI carries, the build number, the MSI's SHA-256 and size); the script checks it against the MSI and the version and does not publish it. `released_at` and the other dates in the feeds are the time of publication, never a planned one.

**Build number.** `apps/macos/scripts/build-number.sh` turns a version into `major * 10000 + minor * 100 + patch` (1.0.0 is 10000, 1.2.3 is 10203), refusing a minor or patch of 100 or more. It is the macOS app's `CFBundleVersion`, which Sparkle compares, and the Windows feed's `build`. It is derived in the build (`package.sh` passes it to xcodebuild, the build phase that embeds the agent writes it into every build), so it grows with every version and nobody keeps it by hand.

### 12.2 macOS

Sparkle 2 with the `.pkg` as the update (`sparkle:installationType="package"`; installing it asks for an administrator's password). Release builds have `SUFeedURL` = `https://inputweave.ctsite.cc/latest/appcast.xml` and `SUPublicEDKey` = `UorCjys4eeYiG7ivmGdNh4KTWhvDdHFumBtYrctpY6o=` (project.yml's Release settings, which the release script also reads); Debug builds have neither and never check. Each appcast item's enclosure points at `https://inputweave.ctsite.cc/download/v<v>/InputWeave-<v>.pkg` (never `latest`) with its EdDSA signature and length. After Sparkle installs the package, the package's own scripts quit and reopen the app, and the new app restarts the agent it finds outdated (10).

### 12.3 Windows: `update-windows.json`

```json
{
  "version": "1.0.1",
  "build": 10001,
  "url": "https://inputweave.ctsite.cc/download/v1.0.1/InputWeave-1.0.1-x64.msi",
  "size": 34567890,
  "sha256": "<64 lowercase hex digits of the MSI>",
  "ed25519": "<base64 of the 64-byte Ed25519 signature over the MSI's bytes>",
  "released_at": "2026-11-04T08:00:00Z",
  "minimum_os": "10.0.19045",
  "notes": "Plain text: the release notes, paragraphs separated by blank lines and items starting with \"- \"."
}
```

- `ed25519` is a plain Ed25519 signature (RFC 8032) over the MSI file, made with the same key as Sparkle's (`sign_update -p`), so it verifies with the public key above, decoded from base64 (32 bytes).
- The app installs an update only when `build` is higher than its own (`version` is for showing), `size` and `sha256` match the downloaded file, and the signature verifies. It downloads to `%LOCALAPPDATA%\InputWeave\updates\<random>`, keeps the file open without sharing write or delete from the check until `msiexec` has read it, and runs `msiexec /i <file> /passive` through `runas`, which shows one UAC prompt; then it quits. The SYSTEM service plays no part: its pipe admits only SYSTEM, and the agent never opens a path a pipe client names (2.2).
- `minimum_os` is the lowest Windows build the MSI installs on (`major.minor.build`); an older Windows is told so instead of being offered the update.
- The app reads the feed at `https://inputweave.ctsite.cc/latest/update-windows.json` and takes an installer only from under `https://inputweave.ctsite.cc/download/` (same scheme, host and port, no user info, nothing escaped in the path); it follows the site's redirect from there.
- Unknown fields are ignored, so later versions can add some.

### 12.4 Linux: the APT repository

The `.deb` installs `/etc/apt/sources.list.d/inputweave.sources`, a conffile:

```
Types: deb
URIs: https://inputweave.ctsite.cc/latest/
Suites: ./
Signed-By:
 -----BEGIN PGP PUBLIC KEY BLOCK-----
 …
```

It is a flat repository (`Suites: ./`, no components) whose files sit beside the packages in each release, so `apt upgrade` and the desktop's software updates install new versions. `Release` lists `Architectures: amd64 arm64`, `Packages` and `Packages.gz` with their sizes and hashes; `InRelease` and `Release.gpg` are its OpenPGP signatures by the repository key `DA65 B795 D2F3 984F 0B30 5013 C3AF 4DF7 6FC8 7E0E` (ed25519, `packaging/linux/inputweave-archive-key.asc`). The key is embedded in the source's `Signed-By` field rather than installed under `/usr/share/keyrings`, because the source stays after `apt remove` until purged and must not lose its key. The Linux app does not check for updates itself.

`Packages` names only its own release's packages, as `./inputweave_<v>_<arch>.deb` under `latest/`, and apt fetches them some time after it read the index, while `latest` may have moved on. So each release also carries the previous release's two packages (checked against that release's `SHA256SUMS`), one release back:

- An index read from the release that is now one behind `latest` still finds its packages.
- An index two or more releases old names packages `latest` no longer holds; installing or upgrading from it fails with a 404 until the next `apt update` reads the current index. Desktops refresh it daily, and `apt update && apt upgrade` always works.
- An `apt update` that reads `InRelease` from one release and `Packages` from the next one, published in between, finds the hashes disagree and reports an error; the next `apt update` reads both from the same release and succeeds.
