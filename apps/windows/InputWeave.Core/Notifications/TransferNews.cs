using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Notifications;

/// <summary>
/// Files and folders from one computer to announce together: those that
/// arrived, and those that failed alongside them, so the notification can
/// say how many did not make it.
/// </summary>
public sealed record ReceivedFiles(string DeviceId, IReadOnlyList<FileTransfer> Files)
{
    /// <summary>Those that arrived.</summary>
    public IReadOnlyList<FileTransfer> Arrived => Files.Where(f => f.State == TransferState.Done).ToList();
}

/// <summary>What the file notifications have to do after a change.</summary>
public sealed record TransferUpdate(
    IReadOnlyList<FileTransfer> Ask,
    IReadOnlyList<string> Withdraw,
    IReadOnlyList<ReceivedFiles> Announce)
{
    public static readonly TransferUpdate Nothing = new([], [], []);

    public bool IsEmpty => Ask.Count == 0 && Withdraw.Count == 0 && Announce.Count == 0;
}

/// <summary>
/// Decides the system notifications for files, as the macOS app's
/// TransferNotifier: one once the files from a
/// computer have all arrived and none has for <see cref="Settle"/>, and one
/// asking about each file when this computer asks before receiving, taken
/// back once the file is answered here or elsewhere. What finished before
/// the app connected is not news, but a file waiting for an answer is: the
/// background service may have started the app to ask. Files and folders a
/// drag or a paste brought are not announced: the user dropped or pasted
/// them where they are.
/// Those that failed go along with the ones that arrived, to be counted,
/// but alone they are no news: the panel shows them.
/// </summary>
public sealed class TransferNews
{
    /// <summary>Files from one computer are announced together once none has arrived for this long.</summary>
    public static readonly TimeSpan Settle = TimeSpan.FromMilliseconds(1500);

    private readonly Dictionary<string, TransferState> states = [];
    private readonly Dictionary<string, (List<FileTransfer> Files, DateTimeOffset Last)> arrived = [];
    private bool primed;

    /// <summary>Takes in the latest state; called whenever it changes, and now and then so announcements go out.</summary>
    public TransferUpdate Update(AgentSnapshot snapshot, DateTimeOffset now)
    {
        var ask = new List<FileTransfer>();
        var withdraw = new List<string>();
        var announce = new List<ReceivedFiles>();
        var incoming = snapshot.Link == AgentLink.Connected
            ? snapshot.Transfers.Transfers_.Where(t => !t.Outgoing && t.DragId.Length == 0 && !t.Clipboard).ToList()
            : null;
        if (incoming is not null && !primed)
        {
            primed = true;
            foreach (var transfer in incoming)
            {
                states[transfer.TransferId] = transfer.State;
                if (transfer.State == TransferState.WaitingForAnswer)
                {
                    ask.Add(transfer);
                }
            }
            return new TransferUpdate(ask, withdraw, announce);
        }
        if (incoming is not null)
        {
            foreach (var transfer in incoming)
            {
                bool known = states.TryGetValue(transfer.TransferId, out var before);
                states[transfer.TransferId] = transfer.State;
                if (known && before == transfer.State)
                {
                    continue;
                }
                if (known && before == TransferState.WaitingForAnswer)
                {
                    withdraw.Add(transfer.TransferId);
                }
                switch (transfer.State)
                {
                    case TransferState.WaitingForAnswer:
                        ask.Add(transfer);
                        break;
                    case TransferState.Done or TransferState.Failed:
                        if (!arrived.TryGetValue(transfer.DeviceId, out var group))
                        {
                            group = ([], now);
                        }
                        group.Files.Add(transfer);
                        arrived[transfer.DeviceId] = (group.Files, now);
                        break;
                }
            }
            // Forget transfers the service no longer lists.
            var listed = incoming.Select(t => t.TransferId).ToHashSet();
            foreach (var gone in states.Keys.Where(id => !listed.Contains(id)).ToList())
            {
                if (states[gone] == TransferState.WaitingForAnswer)
                {
                    withdraw.Add(gone);
                }
                states.Remove(gone);
            }
        }
        foreach (var (device, group) in arrived.ToList())
        {
            bool coming = incoming?.Any(t => t.DeviceId == device && !t.State.IsFinished()) ?? false;
            if (!coming && now - group.Last >= Settle)
            {
                arrived.Remove(device);
                if (group.Files.Any(f => f.State == TransferState.Done))
                {
                    announce.Add(new ReceivedFiles(device, group.Files));
                }
            }
        }
        return new TransferUpdate(ask, withdraw, announce);
    }
}
