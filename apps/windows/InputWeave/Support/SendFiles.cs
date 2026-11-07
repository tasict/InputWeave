using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace InputWeave.Support;

/// <summary>
/// "Send Files…": Windows' file picker, then the
/// computer to send them to when more than one can receive them. The
/// picker has no room for that choice, unlike the Mac's open panel. The
/// choice (<see cref="Deliver"/>, also Send To's) lists the computers files
/// can't go to as well, off, with why, as the Mac's panel for Finder's
/// Services menu does.
/// </summary>
internal static class SendFiles
{
    /// <param name="owner">The window the picker belongs to.</param>
    public static async void Start(nint owner)
    {
        var model = App.Instance.Model;
        // A guard for a race: the buttons that start this are off while no
        // computer can take files, so only a computer going offline as one
        // is pressed gets here. Then, as Send To says it, there is nowhere
        // to send to.
        if (SendTargets.Choose(model.Snapshot) is SendChoice.Nowhere)
        {
            DialogWindow.Notice(Strings.Get(TextKeys.SendNowhere), null);
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
        // Where they go now that they are chosen.
        Deliver(paths);
    }

    /// <summary>
    /// Where <paramref name="paths"/> go: into the picker already open,
    /// which gets them too; else, with no computer connected, a notice; at
    /// once to the only one that can take them; or the picker
    /// (<see cref="SendPicker"/>), listing the others too, off, with why.
    /// <paramref name="sent"/> runs once they are on their way.
    /// </summary>
    public static void Deliver(IReadOnlyList<string> paths, Action? sent = null)
    {
        if (SendPicker.Current is { } open)
        {
            open.Add(paths, sent);
            return;
        }
        var model = App.Instance.Model;
        switch (SendTargets.Choose(model.Snapshot))
        {
            case SendChoice.Only only:
                Send(only.Id, paths, sent);
                break;
            case SendChoice.Ask:
                SendPicker.Show(paths, () => model.Snapshot, (id, chosen, then) => Send(id, chosen, then), sent);
                break;
            default:
                DialogWindow.Notice(Strings.Get(TextKeys.SendNowhere), null);
                break;
        }
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
