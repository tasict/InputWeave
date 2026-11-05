using InputWeave.Agent;

namespace InputWeave.Panel;

/// <summary>
/// The items of the menu the tray icon's right click and the panel's More
/// button share: what they say and whether they can be chosen now.
/// </summary>
public sealed record MenuState(string PauseKey, bool PauseEnabled, string LockKey, bool LockEnabled, bool IdentifyEnabled)
{
    public static MenuState For(AgentSnapshot model)
    {
        var connected = model.Link == AgentLink.Connected;
        var status = model.Status;
        return new MenuState(
            PauseKey: connected && !status.Sharing ? TextKeys.MenuResumeSharing : TextKeys.MenuPauseSharing,
            PauseEnabled: connected,
            LockKey: status.Locked ? TextKeys.MenuUnlock : TextKeys.MenuLock,
            // While another computer controls this one, its lock is the one
            // that holds the cursor.
            LockEnabled: connected && status.Sharing && !model.ControlledFromElsewhere,
            // Identify Screens asks the service, which tells the others.
            IdentifyEnabled: connected);
    }
}
