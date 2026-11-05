using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Setup;

/// <summary>The steps of first-run setup on Windows.</summary>
public enum SetupStep
{
    Welcome,
    Name,
    /// <summary>Only while the network is set to Public.</summary>
    Network,
    Connect,
    /// <summary>The pairing started in the previous step waits for the code the other computer shows.</summary>
    EnterCode,
    Arrange,
    TryIt,
}

/// <summary>
/// The order of the setup steps and what each one decides. There are seven
/// at most: Windows needs no permissions, the installer having set up the
/// service and the firewall, and the network check only appears while the
/// network is set to Public.
/// </summary>
public static class SetupSteps
{
    /// <summary>The steps, in order, for a network that is or is not set to Public.</summary>
    public static IReadOnlyList<SetupStep> For(bool publicNetwork) =>
        Enum.GetValues<SetupStep>().Where(s => publicNetwork || s != SetupStep.Network).ToList();

    /// <summary>The step's number, from 1, as "Step n of N" shows it.</summary>
    public static int Number(IReadOnlyList<SetupStep> steps, SetupStep step) => IndexOf(steps, step) + 1;

    public static SetupStep? Next(IReadOnlyList<SetupStep> steps, SetupStep step)
    {
        int i = IndexOf(steps, step);
        return i >= 0 && i + 1 < steps.Count ? steps[i + 1] : null;
    }

    public static SetupStep? Previous(IReadOnlyList<SetupStep> steps, SetupStep step)
    {
        int i = IndexOf(steps, step);
        return i > 0 ? steps[i - 1] : null;
    }

    private static int IndexOf(IReadOnlyList<SetupStep> steps, SetupStep step)
    {
        for (int i = 0; i < steps.Count; i++)
        {
            if (steps[i] == step)
            {
                return i;
            }
        }
        return -1;
    }

    /// <summary>
    /// Connecting and entering the code share one view, the pairing flow;
    /// which of the two steps it is follows the phase of the pairing the
    /// flow started: from the code being asked for until the user moves on
    /// from "Paired", it is the code step.
    /// </summary>
    public static SetupStep ForPairing(PairingPhase phase) =>
        phase is PairingPhase.EnterCode or PairingPhase.Verifying or PairingPhase.Paired ? SetupStep.EnterCode : SetupStep.Connect;

    /// <summary>
    /// Whether the user has crossed to another computer since the Try It
    /// step opened: the cursor is on another computer now, or the keyboard
    /// and mouse came back since. The service keeps running from one setup
    /// to the next, so returns from before the step do not count.
    /// </summary>
    public static bool Worked(AgentSnapshot snapshot, ulong returnsBefore) =>
        snapshot.ControllingOther || snapshot.Status.ControlReturns > returnsBefore;

    /// <summary>
    /// The computer to name in the arrangement and Try It steps: the one
    /// just paired, else a connected one, else any; null when there is none.
    /// </summary>
    public static string? OtherName(AgentSnapshot snapshot, string? justPaired)
    {
        var devices = snapshot.Status.Devices;
        var device = devices.FirstOrDefault(d => d.Id == justPaired)
            ?? devices.FirstOrDefault(d => d.State == ConnectionState.Connected)
            ?? devices.FirstOrDefault();
        return device?.Name;
    }

    /// <summary>The step's title.</summary>
    public static string TitleKey(SetupStep step) => step switch
    {
        SetupStep.Name => TextKeys.SetupNameTitle,
        SetupStep.Network => TextKeys.SetupNetworkTitle,
        SetupStep.Connect => TextKeys.SetupConnectTitle,
        SetupStep.EnterCode => TextKeys.SetupCodeTitle,
        SetupStep.Arrange => TextKeys.SetupArrangeTitle,
        SetupStep.TryIt => TextKeys.SetupTryTitle,
        _ => TextKeys.SetupWelcomeTitle,
    };
}

/// <summary>The network categories Windows gives a connected network (NLM_NETWORK_CATEGORY).</summary>
public enum NetworkCategory
{
    Public = 0,
    Private = 1,
    DomainAuthenticated = 2,
}

public static class NetworkCheck
{
    /// <summary>
    /// Whether to warn that other computers may not find this one: a
    /// connected network is set to Public, where Windows keeps a computer
    /// from being found on the network.
    /// </summary>
    public static bool IsPublic(IEnumerable<NetworkCategory> connected) => connected.Contains(NetworkCategory.Public);
}
