using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Pairing;

/// <summary>
/// Add Device: the pairing flow in a dialog over
/// the main window. Closing it cancels a pairing it started that is still
/// in progress.
/// </summary>
internal sealed partial class AddDeviceDialog : ContentDialog
{
    private readonly PairingFlow flow;

    public AddDeviceDialog(AgentModel agent)
    {
        Title = Strings.Get(TextKeys.AddDeviceTitle);
        CloseButtonText = Strings.Get(TextKeys.Cancel);
        DefaultButton = ContentDialogButton.None;
        flow = new PairingFlow(agent) { Width = 440, Height = 320 };
        flow.Finished += (_, _) =>
        {
            Paired = true;
            Hide();
        };
        Closing += (_, _) =>
        {
            if (!Paired)
            {
                flow.CancelInProgress();
            }
        };
        Content = flow;
    }

    /// <summary>Paired, and the user moved on: the window goes to the arrangement.</summary>
    public bool Paired { get; private set; }

    /// <summary>Follows the pairing as the service reports it.</summary>
    public void Update(AgentSnapshot snapshot) => flow.Update(snapshot);
}
