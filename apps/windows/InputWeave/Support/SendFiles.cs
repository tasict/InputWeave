using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace InputWeave.Support;

/// <summary>
/// "Send Files…": Windows' file picker, then the
/// computer to send them to when more than one is connected. The picker
/// has no room for that choice, unlike the Mac's open panel. Computers
/// files are turned off with are not offered.
/// </summary>
internal static class SendFiles
{
    /// <param name="owner">The window the picker belongs to.</param>
    /// <param name="to">The computer to send to, when already chosen.</param>
    public static async void Start(nint owner, string? to = null)
    {
        var model = App.Instance.Model;
        if (model.Snapshot.FileReceivers.Count == 0)
        {
            return;
        }
        var picker = new FileOpenPicker { ViewMode = PickerViewMode.List };
        picker.FileTypeFilter.Add("*");
        InitializeWithWindow.Initialize(picker, owner);
        IReadOnlyList<Windows.Storage.StorageFile> files;
        try
        {
            files = await picker.PickMultipleFilesAsync();
        }
        catch (Exception e)
        {
            AppLog.Warn($"the file picker failed: {e.Message}");
            return;
        }
        if (files is null || files.Count == 0)
        {
            return;
        }
        var paths = files.Select(f => f.Path).Where(p => !string.IsNullOrEmpty(p)).ToList();
        // Still connected now that the files are chosen.
        var devices = model.Snapshot.FileReceivers;
        if (to is not null && devices.Any(d => d.Id == to))
        {
            Send(to, paths);
        }
        else if (devices.Count == 1)
        {
            Send(devices[0].Id, paths);
        }
        else if (devices.Count > 1)
        {
            Choose(devices, paths);
        }
    }

    /// <summary>Asks which connected computer gets the files, then sends them and runs <paramref name="sent"/>.</summary>
    public static void Choose(IReadOnlyList<DeviceStatus> devices, IReadOnlyList<string> paths, Action? sent = null)
    {
        var picker = new ComboBox
        {
            ItemsSource = devices.Select(d => d.Name).ToList(),
            SelectedIndex = 0,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Header = Strings.Get(TextKeys.SendFilesTo),
        };
        new DialogWindow(
            Strings.Get(TextKeys.SendFilesTitle),
            null,
            picker,
            [
                new DialogButton(Strings.Get(TextKeys.Cancel)),
                new DialogButton(
                    Strings.Get(TextKeys.SendFilesSend),
                    () => Send(devices[Math.Max(picker.SelectedIndex, 0)].Id, paths, sent),
                    Accent: true),
            ]).Open();
    }

    public static async void Send(string to, IReadOnlyList<string> paths, Action? sent = null)
    {
        var model = App.Instance.Model;
        try
        {
            await model.SendFilesAsync(to, paths);
            AppLog.Info($"sending {paths.Count} files to {to}");
            sent?.Invoke();
        }
        catch (Exception e)
        {
            AppLog.Info($"send files to {to}: {e.Message}");
            ShowFailure(e, model.Snapshot.NameOf(to));
        }
    }

    /// <summary>Says why files could not go to a computer.</summary>
    public static void ShowFailure(Exception error, string device) =>
        DialogWindow.Notice(
            Strings.Format(TextKeys.SendFilesFailed, device),
            error switch
            {
                AgentRefusal { Code: "not_a_file" } => Strings.Get(TextKeys.SendFilesOnlyFiles),
                AgentRefusal { Code: "files_off" } => Strings.Text(DeviceText.FilesOff(device)),
                _ => Strings.Format(TextKeys.SendFilesCheckConnected, device),
            });
}
