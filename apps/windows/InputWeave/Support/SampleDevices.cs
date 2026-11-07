using Inputweave.App.V1;
using InputWeave.Agent;

namespace InputWeave.Support;

/// <summary>
/// The computers as a background service that numbers them for the jump
/// shortcuts would report them, for screenshots against one that does not
/// yet (<see cref="Launch.ShowSampleDevicesArgument"/>): every placed
/// computer numbered from left to right, then from top to bottom, and the
/// first other computer named "Studio Mac" here, with its own name as the
/// name it gives itself. A service without relative mode has its hotkey
/// and its switch, as a newer one starts out with.
/// </summary>
internal static class SampleDevices
{
    public const string Alias = "Studio Mac";

    public static AgentSnapshot Over(AgentSnapshot snapshot)
    {
        var arrangement = snapshot.Arrangement.Clone();
        var placed = arrangement.Devices
            .Where(d => d.Placed && d.Screens.Count != 0)
            .OrderBy(d => d.X + d.Screens.Min(s => s.X))
            .ThenBy(d => d.Y + d.Screens.Min(s => s.Y))
            .Take(9)
            .ToList();
        for (int i = 0; i < placed.Count; i++)
        {
            placed[i].JumpSlot = (uint)(i + 1);
        }
        var status = snapshot.Status.Clone();
        foreach (var device in status.Devices)
        {
            device.JumpSlot = arrangement.Devices.FirstOrDefault(d => d.Id == device.Id)?.JumpSlot ?? 0;
            if (device.ReportedName.Length == 0)
            {
                device.ReportedName = device.Name;
            }
        }
        if (status.Devices.FirstOrDefault() is { } renamed)
        {
            renamed.Name = Alias;
            foreach (var arranged in arrangement.Devices.Where(d => d.Id == renamed.Id))
            {
                arranged.Name = Alias;
            }
        }
        if (!status.RelativeSupported)
        {
            var hotkeys = status.Hotkeys?.Clone() ?? Shortcut.Defaults;
            hotkeys.Relative = Shortcut.DefaultRelative;
            status.Hotkeys = hotkeys;
            status.RelativeAuto = true;
            status.RelativeSupported = true;
        }
        return snapshot with { Arrangement = arrangement, Status = status };
    }

    /// <summary>
    /// Made-up computers for the picker of where files go
    /// (<see cref="Launch.ShowSendPickerArgument"/>): two that can take
    /// them, one offline and one files are turned off with.
    /// </summary>
    public static AgentSnapshot Sending()
    {
        var status = new Status { ThisDeviceId = "pc", ThisDeviceName = "Windows Desktop" };
        status.Devices.Add(new DeviceStatus { Id = "mac", Name = "MacBook Pro", Os = "macos", State = ConnectionState.Connected, RttMs = 3 });
        status.Devices.Add(new DeviceStatus { Id = "mini", Name = "Mac mini", Os = "macos", State = ConnectionState.Offline });
        status.Devices.Add(new DeviceStatus { Id = "media", Name = "Living Room PC", Os = "windows", State = ConnectionState.Connected, RttMs = 7, FilesOff = true });
        status.Devices.Add(new DeviceStatus { Id = "laptop", Name = "ThinkPad", Os = "linux", State = ConnectionState.Connected, RttMs = 5 });
        return new AgentSnapshot { Link = AgentLink.Connected, Status = status };
    }
}
