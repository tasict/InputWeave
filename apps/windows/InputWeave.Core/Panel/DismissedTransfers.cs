using InputWeave.Agent;

namespace InputWeave.Panel;

/// <summary>
/// The transfer problems the panel no longer lists: those dismissed with ×, and those already in the history
/// when the app first connects, which are no more news than the files
/// <see cref="InputWeave.Notifications.TransferNews"/> leaves unannounced then. The
/// history outlives the app, so the app keeps this set too
/// (<c>HKCU\Software\InputWeave\App</c>, <c>DismissedTransfers</c>), and
/// forgets the ids the service no longer lists, so the set never grows
/// past the history.
/// </summary>
public sealed class DismissedTransfers
{
    private readonly HashSet<string> ids;
    private bool primed;

    /// <param name="saved">The ids the app kept from before.</param>
    public DismissedTransfers(IEnumerable<string> saved)
    {
        ids = new HashSet<string>(saved.Where(id => id.Length != 0), StringComparer.Ordinal);
    }

    public IReadOnlySet<string> Ids => ids;

    /// <summary>Goes up whenever <see cref="Ids"/> changes, to tell what the panel shows is out of date.</summary>
    public int Version { get; private set; }

    /// <summary>The user dismissed a problem. True when it was not dismissed yet, so the set is worth saving.</summary>
    public bool Dismiss(string transferId)
    {
        if (!ids.Add(transferId))
        {
            return false;
        }
        Version++;
        return true;
    }

    /// <summary>
    /// Takes in the latest state; called whenever it changes. True when the
    /// set changed and is worth saving. Only a connected service's list
    /// counts: while it is out of reach, nothing is known to be gone.
    /// </summary>
    public bool Update(AgentSnapshot snapshot)
    {
        if (snapshot.Link != AgentLink.Connected)
        {
            return false;
        }
        var transfers = snapshot.Transfers.Transfers_;
        bool changed = false;
        if (!primed)
        {
            primed = true;
            foreach (var transfer in transfers.Where(TransferText.IsProblem))
            {
                changed |= ids.Add(transfer.TransferId);
            }
        }
        var listed = transfers.Select(t => t.TransferId).ToHashSet(StringComparer.Ordinal);
        changed |= ids.RemoveWhere(id => !listed.Contains(id)) != 0;
        if (changed)
        {
            Version++;
        }
        return changed;
    }
}
