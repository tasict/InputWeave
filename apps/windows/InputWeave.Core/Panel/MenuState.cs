using InputWeave.Agent;
using InputWeave.Settings;

namespace InputWeave.Panel;

/// <summary>
/// The items of the menu the tray icon's right click and the panel's More
/// button share: what they say and whether they can be chosen now.
/// <paramref name="OfflineKey"/> takes this computer offline for now or
/// brings it back; null leaves the item out, for a service that does not
/// know it. <paramref name="Profiles"/> is the Profile submenu; null leaves
/// it out, for a service that does not know profiles.
/// </summary>
public sealed record MenuState(
    string PauseKey, bool PauseEnabled, string LockKey, bool LockEnabled, bool IdentifyEnabled,
    string? OfflineKey = null, bool OfflineEnabled = false, ProfileMenu? Profiles = null)
{
    public static MenuState For(AgentSnapshot model)
    {
        var connected = model.Link == AgentLink.Connected;
        var status = model.Status;
        var offline = OfflineForNow.MenuKey(model);
        // In a paused profile, sharing comes back by changing the profile.
        bool profilePaused = Settings.Profiles.Paused(model);
        return new MenuState(
            PauseKey: connected && !status.Sharing ? TextKeys.MenuResumeSharing : TextKeys.MenuPauseSharing,
            PauseEnabled: connected && !profilePaused,
            LockKey: status.Locked ? TextKeys.MenuUnlock : TextKeys.MenuLock,
            // While another computer controls this one, its lock is the one
            // that holds the cursor; offline for now, or in a paused profile,
            // the cursor goes nowhere.
            LockEnabled: connected && status.Sharing && !model.ControlledFromElsewhere && !OfflineForNow.ThisOffline(model)
                && !profilePaused,
            // Identify Screens asks the service, which tells the others.
            IdentifyEnabled: connected,
            OfflineKey: offline,
            OfflineEnabled: connected && offline is not null,
            Profiles: Settings.Profiles.Menu(model));
    }
}
